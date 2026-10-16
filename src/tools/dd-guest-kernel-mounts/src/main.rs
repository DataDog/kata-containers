// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! Kata guest OCI prestart hook. For the GitLab docker-in-docker service
//! container of a pod that opted in with the
//! `io.katacontainers.datadog.guest-kernel-mounts=v1` annotation, it clones the
//! guest-initial `/proc`, `/sys/fs/cgroup`, `/sys/kernel/debug`,
//! `/sys/kernel/tracing` and `/sys/kernel/security` mounts into `/vm-host`
//! inside that container's rootfs, then writes `/vm-host/.ready-v1`. Every other
//! container is a silent no-op. See README.md for the contract and threat model.

mod mount;

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::ExitCode;

use rustix::fs::{Mode, OFlags};
use serde::Deserialize;

pub const HOOK_NAME: &str = "dd-guest-kernel-mounts";

// annotation_key matches containerd's pod_annotations allowlist for the Kata
// handlers (io.katacontainers.*), so containerd copies it from the pod into the
// container spec. Kata itself ignores it.
pub const ANNOTATION_KEY: &str = "io.katacontainers.datadog.guest-kernel-mounts";
pub const ANNOTATION_VALUE: &str = "v1";

const CRI_CONTAINER_TYPE: &str = "io.kubernetes.cri.container-type";
const CRI_CONTAINER_NAME: &str = "io.kubernetes.cri.container-name";
// The GitLab runner names the injected DIND service "docker". The target is
// fixed here, not chosen by the job.
const TARGET_TYPE: &str = "container";
const TARGET_NAME: &str = "docker";

const MAX_DOCUMENT: usize = 1 << 20;

pub const VM_HOST_DIR: &str = "vm-host";
pub const READY_MARKER: &str = "vm-host/.ready-v1";

const BUNDLE_BASE: &str = "/run/kata-containers";

/// A guest kernel mount exposed under `/vm-host`.
#[derive(Clone, Copy)]
pub struct Filesystem {
    /// guest path, in the hook's (guest-initial) mount namespace
    pub source: &'static str,
    /// path below /vm-host in the container rootfs
    pub target: &'static str,
    /// filesystem to mount when `mount_if_missing` and nothing is there yet
    pub fstype: &'static str,
    /// statfs f_type the source must have
    pub magic: i64,
    /// adds MOUNT_ATTR_RDONLY; every clone is nosuid,nodev,noexec regardless
    pub read_only: bool,
    /// mount `fstype` at `source` first if nothing is mounted there; the guest
    /// does not mount these pseudo filesystems by default
    pub mount_if_missing: bool,
}

/// Layout v1. Order matters only for logging.
pub const FILESYSTEMS: &[Filesystem] = &[
    Filesystem { source: "/proc", target: "proc", fstype: "proc", magic: 0x9fa0, read_only: true, mount_if_missing: false },
    Filesystem { source: "/sys/fs/cgroup", target: "sys/fs/cgroup", fstype: "cgroup2", magic: 0x6367_7270, read_only: true, mount_if_missing: false },
    Filesystem { source: "/sys/kernel/debug", target: "sys/kernel/debug", fstype: "debugfs", magic: 0x6462_6720, read_only: false, mount_if_missing: true },
    Filesystem { source: "/sys/kernel/tracing", target: "sys/kernel/tracing", fstype: "tracefs", magic: 0x7472_6163, read_only: false, mount_if_missing: true },
    Filesystem { source: "/sys/kernel/security", target: "sys/kernel/security", fstype: "securityfs", magic: 0x7363_6673, read_only: false, mount_if_missing: true },
];

/// The subset of the OCI runtime state kata-agent writes to stdin.
#[derive(Deserialize)]
struct State {
    #[serde(default)]
    id: String,
    #[serde(default)]
    pid: i64,
    #[serde(default)]
    status: String,
    #[serde(default)]
    bundle: String,
    // Absent or null annotations are treated as empty, as a nil Go map would be.
    #[serde(default, deserialize_with = "null_as_default")]
    annotations: HashMap<String, String>,
}

fn null_as_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

impl State {
    fn annotation(&self, key: &str) -> Option<&str> {
        self.annotations.get(key).map(String::as_str)
    }
}

/// What `evaluate` decided the hook must do for the opted-in DIND container.
#[derive(Debug)]
pub struct Plan {
    pub id: String,
    pub pid: i64,
    pub root_path: String,
}

/// Everything that differs between production and unit tests. Production always
/// uses `default_host()`; the binary takes no flags.
pub struct Host {
    pub(crate) bundle_base: String,
    pub(crate) proc_root: String,
    pub(crate) kmsg: String,
    pub(crate) sources: &'static [Filesystem],
    pub(crate) out: Box<dyn Write>,
    // Opened once, before setns, so later records do not reopen /dev/kmsg by a
    // path that would resolve inside the container.
    pub(crate) kmsg_file: Option<File>,
}

pub fn default_host() -> Host {
    Host {
        bundle_base: BUNDLE_BASE.to_string(),
        proc_root: "/proc".to_string(),
        kmsg: "/dev/kmsg".to_string(),
        sources: FILESYSTEMS,
        out: Box::new(io::stdout()),
        kmsg_file: None,
    }
}

impl Host {
    /// Opens the kernel log for later records, best effort.
    pub(crate) fn open_kmsg(&mut self) {
        if self.kmsg_file.is_some() || self.kmsg.is_empty() {
            return;
        }
        if let Ok(fd) = rustix::fs::open(&self.kmsg, OFlags::WRONLY | OFlags::CLOEXEC, Mode::empty()) {
            self.kmsg_file = Some(File::from(fd));
        }
    }

    /// Writes one best-effort record to the kernel log.
    pub(crate) fn kmsg_line(&mut self, msg: &str) {
        self.open_kmsg();
        if let Some(f) = self.kmsg_file.as_mut() {
            let _ = writeln!(f, "{HOOK_NAME}: {msg}");
        }
    }
}

pub(crate) fn err(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}

fn main() -> ExitCode {
    // kata-agent passes [name, "prestart"] as argv. It is ignored.
    let mut host = default_host();
    let result = run(io::stdin().lock(), &mut host, mount::install_mounts);
    ExitCode::from(report(result, &mut host, io::stderr()) as u8)
}

/// Turns the result into an exit status. kata-agent runs guest prestart hooks
/// from its own process and records their stderr in the agent log, which
/// operators rarely have at hand. The reason is therefore also written to the
/// kernel log, next to kata-agent's "kata-agent: hook failed" line, where a
/// guest `dmesg` (the documented diagnostic) shows both.
pub fn report(result: io::Result<()>, host: &mut Host, mut stderr: impl Write) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => {
            let msg = collapse_whitespace(&e.to_string());
            let _ = writeln!(stderr, "{HOOK_NAME}: {msg}");
            host.kmsg_line(&msg);
            1
        }
    }
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Decodes the state, applies the gate and validation, and runs `install` only
/// for the opted-in DIND container.
pub fn run<F>(input: impl Read, host: &mut Host, install: F) -> io::Result<()>
where
    F: FnOnce(&mut Host, &Plan) -> io::Result<()>,
{
    match evaluate(input, host)? {
        Some(plan) => install(host, &plan),
        None => Ok(()),
    }
}

fn evaluate(input: impl Read, host: &Host) -> io::Result<Option<Plan>> {
    let state = decode_state(input)?;
    if !opted_in(&state)? {
        return Ok(None);
    }
    validate_state(&state, &host.bundle_base)?;
    // kata-agent always prepares the rootfs at <bundle>/rootfs and names it in
    // config.json's root.path. With the bundle pinned to /run/kata-containers/<id>
    // the rootfs path is therefore fixed, so the hook derives it rather than
    // reading and re-validating the bundle's config.json.
    let root_path = format!("{}/rootfs", state.bundle);
    Ok(Some(Plan { id: state.id, pid: state.pid, root_path }))
}

/// The cheap gate every container in the cluster goes through. It must not touch
/// the filesystem. Returns Ok(false) for a silent no-op, Ok(true) to act, and
/// an error only when the target container carries an unsupported value.
fn opted_in(s: &State) -> io::Result<bool> {
    if s.annotation(CRI_CONTAINER_TYPE) != Some(TARGET_TYPE)
        || s.annotation(CRI_CONTAINER_NAME) != Some(TARGET_NAME)
    {
        return Ok(false);
    }
    match s.annotation(ANNOTATION_KEY) {
        None => Ok(false),
        Some(ANNOTATION_VALUE) => Ok(true),
        Some(other) => Err(err(format!("unsupported {ANNOTATION_KEY} value {other:?}"))),
    }
}

fn validate_state(s: &State, bundle_base: &str) -> io::Result<()> {
    if s.status != "created" {
        return Err(err(format!("container status is {:?}, expected \"created\"", s.status)));
    }
    if s.pid <= 1 {
        return Err(err(format!("container PID {} is not a guest process", s.pid)));
    }
    if !valid_id(&s.id) {
        return Err(err(format!("invalid container ID {:?}", s.id)));
    }
    // kata-agent always places the bundle at <bundle_base>/<id>.
    let want = Path::new(bundle_base).join(&s.id);
    if Path::new(&s.bundle) != want {
        return Err(err(format!("bundle {:?} is not {:?}", s.bundle, want.to_string_lossy())));
    }
    let real = std::fs::canonicalize(&s.bundle)
        .map_err(|e| err(format!("resolve bundle: {e}")))?;
    if real != Path::new(&s.bundle) {
        return Err(err(format!("bundle {:?} has symlink components", s.bundle)));
    }
    Ok(())
}

/// Mirrors `^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$`: a container ID is a non-empty,
/// at most 128 character token that cannot be a path component like "." or "..".
fn valid_id(id: &str) -> bool {
    let b = id.as_bytes();
    if b.is_empty() || b.len() > 128 {
        return false;
    }
    if !b[0].is_ascii_alphanumeric() {
        return false;
    }
    b[1..]
        .iter()
        .all(|&c| c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c == b'-')
}

/// Decodes a single OCI state document of at most `MAX_DOCUMENT` bytes.
fn decode_state(input: impl Read) -> io::Result<State> {
    let mut buf = Vec::new();
    input.take((MAX_DOCUMENT + 1) as u64).read_to_end(&mut buf)?;
    if buf.len() > MAX_DOCUMENT {
        return Err(err(format!("state document exceeds {MAX_DOCUMENT} bytes")));
    }
    serde_json::from_slice(&buf).map_err(|e| err(format!("decode OCI state: {e}")))
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    const TEST_ID: &str = "0123456789abcdef";

    struct TmpDir {
        path: PathBuf,
    }
    impl TmpDir {
        fn new() -> TmpDir {
            static CTR: AtomicU64 = AtomicU64::new(0);
            let n = CTR.fetch_add(1, Ordering::SeqCst);
            let p = std::env::temp_dir().join(format!("ddgkm-{}-{}", std::process::id(), n));
            std::fs::create_dir_all(&p).unwrap();
            // canonicalize resolves the /var -> /private/var symlink on macOS.
            TmpDir { path: std::fs::canonicalize(&p).unwrap() }
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn test_host(base: &Path) -> Host {
        Host {
            bundle_base: base.to_string_lossy().into_owned(),
            proc_root: "/nonexistent-proc".into(),
            kmsg: String::new(),
            sources: FILESYSTEMS,
            out: Box::new(io::sink()),
            kmsg_file: None,
        }
    }

    fn opted_in() -> Value {
        json!({
            ANNOTATION_KEY: ANNOTATION_VALUE,
            CRI_CONTAINER_NAME: TARGET_NAME,
            CRI_CONTAINER_TYPE: TARGET_TYPE,
        })
    }

    fn state_json(bundle: &Path, annotations: Value, pid: i64, status: &str, id: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "id": id,
            "pid": pid,
            "status": status,
            "bundle": bundle.to_string_lossy(),
            "annotations": annotations,
        }))
        .unwrap()
    }

    #[test]
    fn opted_in_dind_reaches_a_plan() {
        let base = TmpDir::new();
        let bundle = base.path.join(TEST_ID);
        std::fs::create_dir_all(bundle.join("rootfs")).unwrap();
        let host = test_host(&base.path);
        let js = state_json(&bundle, opted_in(), 4242, "created", TEST_ID);
        let plan = evaluate(&js[..], &host).unwrap().expect("expected a plan");
        assert_eq!((plan.id.as_str(), plan.pid), (TEST_ID, 4242));
        assert_eq!(plan.root_path, bundle.join("rootfs").to_string_lossy());
    }

    #[test]
    fn non_target_container_is_a_silent_noop() {
        // A build container in the same opted-in pod: the gate skips it without
        // touching the (absent) bundle.
        let base = TmpDir::new();
        let host = test_host(&base.path);
        let ann = json!({
            ANNOTATION_KEY: ANNOTATION_VALUE,
            CRI_CONTAINER_NAME: "build",
            CRI_CONTAINER_TYPE: TARGET_TYPE,
        });
        let js = state_json(&base.path.join(TEST_ID), ann, 4242, "created", TEST_ID);
        assert!(evaluate(&js[..], &host).unwrap().is_none());
    }

    #[test]
    fn unsupported_opt_in_value_fails() {
        let base = TmpDir::new();
        let host = test_host(&base.path);
        let ann = json!({
            ANNOTATION_KEY: "v2",
            CRI_CONTAINER_NAME: TARGET_NAME,
            CRI_CONTAINER_TYPE: TARGET_TYPE,
        });
        let js = state_json(&base.path.join(TEST_ID), ann, 4242, "created", TEST_ID);
        let e = evaluate(&js[..], &host).expect_err("accepted v2");
        assert!(e.to_string().contains("unsupported"), "{e}");
    }

    #[test]
    fn invalid_state_is_rejected() {
        let base = TmpDir::new();
        let bundle = base.path.join(TEST_ID);
        std::fs::create_dir_all(bundle.join("rootfs")).unwrap();
        let host = test_host(&base.path);
        // A non-created status, PID <= 1, and a traversal ID are each refused.
        for (pid, status, id) in [
            (0_i64, "created", TEST_ID),
            (4242, "running", TEST_ID),
            (4242, "created", "../escape"),
        ] {
            let js = state_json(&bundle, opted_in(), pid, status, id);
            assert!(evaluate(&js[..], &host).is_err(), "accepted pid={pid} status={status} id={id}");
        }
    }

    #[test]
    fn symlinked_bundle_is_rejected() {
        let base = TmpDir::new();
        let bundle = base.path.join(TEST_ID);
        let real = base.path.join("real");
        std::fs::create_dir_all(real.join("rootfs")).unwrap();
        std::os::unix::fs::symlink(&real, &bundle).unwrap();
        let host = test_host(&base.path);
        let js = state_json(&bundle, opted_in(), 4242, "created", TEST_ID);
        let e = evaluate(&js[..], &host).expect_err("accepted a symlinked bundle");
        assert!(e.to_string().contains("symlink"), "{e}");
    }

    #[test]
    fn malformed_and_oversized_state_rejected() {
        let base = TmpDir::new();
        let host = test_host(&base.path);
        assert!(evaluate(&b""[..], &host).is_err(), "accepted empty input");
        assert!(evaluate(&b"{"[..], &host).is_err(), "accepted malformed JSON");
        let big = vec![b' '; MAX_DOCUMENT + 1];
        assert!(evaluate(&big[..], &host).is_err(), "accepted an oversized document");
    }
}
