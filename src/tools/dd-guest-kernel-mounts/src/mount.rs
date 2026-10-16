// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! The privileged install path: clone the guest kernel mounts and attach them
//! under `/vm-host` inside the target container's rootfs.

// The one raw syscall below (mount_setattr) uses a number that is the same on
// amd64 and arm64; the Datadog guest is built for no other architecture.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("dd-guest-kernel-mounts supports only linux/amd64 and linux/arm64");

use std::fs;
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::raw::{c_char, c_long};

use rustix::fs::{fstatfs, mkdirat, open, openat, statfs, Mode, OFlags, CWD};
use rustix::io::Errno;
use rustix::mount::{mount, move_mount, open_tree, MountFlags, MoveMountFlags, OpenTreeFlags};
use rustix::thread::{move_into_link_name_space, LinkNameSpaceType};

use crate::{err, Filesystem, Host, Plan, READY_MARKER, VM_HOST_DIR};

// mount_setattr is the one call rustix does not wrap, so it keeps a single raw
// syscall. Its number is the same on x86_64 and arm64 (unified table).
const SYS_MOUNT_SETATTR: c_long = 442;
const AT_EMPTY_PATH: c_long = 0x1000;
const MOUNT_ATTR_RDONLY: u64 = 0x1;
const MOUNT_ATTR_NOSUID: u64 = 0x2;
const MOUNT_ATTR_NODEV: u64 = 0x4;
const MOUNT_ATTR_NOEXEC: u64 = 0x8;

// struct mount_attr (MOUNT_ATTR_SIZE_VER0).
#[repr(C)]
struct MountAttr {
    attr_set: u64,
    attr_clr: u64,
    propagation: u64,
    userns_fd: u64,
}

extern "C" {
    fn syscall(num: c_long, ...) -> c_long;
}

fn os(ctx: impl FnOnce() -> String, e: Errno) -> io::Error {
    let e = io::Error::from(e);
    io::Error::new(e.kind(), format!("{}: {e}", ctx()))
}

/// Runs after the state has been validated. Until the target's mount namespace
/// is entered it changes nothing except mounting missing
/// debugfs/tracefs/securityfs instances at their usual guest paths.
pub fn install_mounts(host: &mut Host, plan: &Plan) -> io::Result<()> {
    let sources = host.sources;
    check_namespaces(&host.proc_root, plan.pid)?;
    prepare_sources(sources)?;

    // Detached clones with their final attributes, created before leaving the
    // hook's own mount namespace. Each OwnedFd closes itself on return.
    let mut clones: Vec<OwnedFd> = Vec::with_capacity(sources.len());
    for fs in sources {
        clones.push(clone_mount(fs)?);
    }

    let pid_dir = format!("{}/{}", host.proc_root, plan.pid);
    let nsfd = open(format!("{pid_dir}/ns/mnt"), OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty())
        .map_err(|e| os(|| format!("open {pid_dir}/ns/mnt"), e))?;
    // At guest prestart the container has its own mount namespace but has not
    // pivoted, so its root is still the guest root and root.path names the
    // prepared rootfs below it. Resolve it from the task's root without
    // following symlinks.
    let task_root = open(
        format!("{pid_dir}/root"),
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| os(|| format!("open {pid_dir}/root"), e))?;
    let rootfd = open_directory_at(task_root.as_fd(), &plan.root_path, false)
        .map_err(|e| err(format!("open target rootfs {}: {e}", plan.root_path)))?;

    // move_mount only attaches to mounts in the caller's namespace, and paths
    // resolve inside the container from here on.
    host.open_kmsg();
    enter_mount_namespace(nsfd.as_fd())?;
    reserve_vm_host(rootfd.as_fd())?;
    // From here a failure leaves /vm-host without its marker, which consumers
    // must treat as "not ready". Attached mounts are not undone.
    for (fs, clone) in sources.iter().zip(&clones) {
        attach(rootfd.as_fd(), fs, clone.as_fd())?;
        let _ = writeln!(host.out, "mounted {} -> /{}/{}", fs.source, VM_HOST_DIR, fs.target);
    }
    write_marker(rootfd.as_fd(), &plan.id)?;
    host.kmsg_line(&format!(
        "exposed {} guest kernel mounts at /{} in container {}",
        sources.len(),
        VM_HOST_DIR,
        plan.id
    ));
    Ok(())
}

/// Enforces that the hook runs where kata-agent runs guest hooks (the
/// guest-initial PID, cgroup and user namespaces) and that the target is an
/// ordinary, non-userns-remapped container with its own mount namespace.
fn check_namespaces(proc_root: &str, pid: i64) -> io::Result<()> {
    let self_ns = format!("{proc_root}/self/ns");
    let init_ns = format!("{proc_root}/1/ns");
    let target_ns = format!("{proc_root}/{pid}/ns");
    for kind in ["pid", "cgroup", "user"] {
        let own = fs::read_link(format!("{self_ns}/{kind}"))?;
        let pid1 = fs::read_link(format!("{init_ns}/{kind}"))?;
        if own != pid1 {
            return Err(err(format!("hook is not in guest PID 1's {kind} namespace")));
        }
    }
    let own_user = fs::read_link(format!("{self_ns}/user"))?;
    let target_user = fs::read_link(format!("{target_ns}/user"))
        .map_err(|e| err(format!("read target user namespace: {e}")))?;
    if target_user != own_user {
        return Err(err("refusing a user-namespace-remapped target"));
    }
    let own_mnt = fs::read_link(format!("{self_ns}/mnt"))?;
    let target_mnt = fs::read_link(format!("{target_ns}/mnt"))
        .map_err(|e| err(format!("read target mount namespace: {e}")))?;
    if target_mnt == own_mnt {
        return Err(err("target shares the hook's mount namespace"));
    }
    Ok(())
}

/// Makes sure each source is the expected filesystem, mounting it first where
/// the guest leaves it unmounted.
fn prepare_sources(sources: &[Filesystem]) -> io::Result<()> {
    for fs in sources {
        let mut magic = statfs_magic(fs.source)?;
        if magic != fs.magic && fs.mount_if_missing {
            mount(
                fs.fstype,
                fs.source,
                fs.fstype,
                MountFlags::NOSUID | MountFlags::NODEV | MountFlags::NOEXEC,
                None::<&core::ffi::CStr>,
            )
            .map_err(|e| os(|| format!("mount {} at {}", fs.fstype, fs.source), e))?;
            magic = statfs_magic(fs.source)?;
        }
        if magic != fs.magic {
            return Err(err(format!(
                "{} is filesystem {magic:#x}, expected {} ({:#x})",
                fs.source, fs.fstype, fs.magic
            )));
        }
    }
    Ok(())
}

fn statfs_magic(path: &str) -> io::Result<i64> {
    let st = statfs(path).map_err(|e| os(|| format!("statfs {path}"), e))?;
    Ok(st.f_type as i64)
}

/// Creates a detached, non-recursive copy of the source with the final mount
/// attributes, before the hook leaves its own namespace.
fn clone_mount(fs: &Filesystem) -> io::Result<OwnedFd> {
    let clone = open_tree(CWD, fs.source, OpenTreeFlags::OPEN_TREE_CLONE | OpenTreeFlags::OPEN_TREE_CLOEXEC)
        .map_err(|e| os(|| format!("open_tree {}", fs.source), e))?;
    // Check the clone itself, not only the path checked earlier.
    let st = fstatfs(&clone).map_err(|e| os(|| format!("fstatfs clone of {}", fs.source), e))?;
    if st.f_type as i64 != fs.magic {
        return Err(err(format!("clone is filesystem {:#x}, expected {:#x}", st.f_type, fs.magic)));
    }
    let mut attr_set = MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV | MOUNT_ATTR_NOEXEC;
    if fs.read_only {
        attr_set |= MOUNT_ATTR_RDONLY;
    }
    mount_setattr(clone.as_fd(), attr_set)
        .map_err(|e| io::Error::new(e.kind(), format!("mount_setattr {}: {e}", fs.source)))?;
    Ok(clone)
}

/// The single raw syscall rustix does not provide. Sets the attributes on an
/// already-opened mount fd with the empty relative path + AT_EMPTY_PATH.
fn mount_setattr(fd: BorrowedFd, attr_set: u64) -> io::Result<()> {
    let attr = MountAttr { attr_set, attr_clr: 0, propagation: 0, userns_fd: 0 };
    let empty = c"".as_ptr() as *const c_char;
    let r = unsafe {
        syscall(
            SYS_MOUNT_SETATTR,
            fd.as_raw_fd() as c_long,
            empty,
            AT_EMPTY_PATH,
            &attr as *const MountAttr,
            core::mem::size_of::<MountAttr>() as c_long,
        )
    };
    // errno captured as the first thing after the call.
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Moves the calling thread into the target's mount namespace. The process is
/// single-threaded, so setns(CLONE_NEWNS) needs neither a locked OS thread nor
/// unshare(CLONE_FS): nothing shares this thread's fs_struct and no scheduler
/// can migrate the work afterwards. A future dependency that spawns a thread
/// before this point would quietly break that assumption.
fn enter_mount_namespace(nsfd: BorrowedFd) -> io::Result<()> {
    move_into_link_name_space(nsfd, Some(LinkNameSpaceType::Mount))
        .map_err(|e| os(|| "enter target mount namespace".to_string(), e))
}

/// Claims /vm-host for this hook. An existing entry is an error, which also
/// keeps a repeated invocation from stacking mounts.
fn reserve_vm_host(rootfd: BorrowedFd) -> io::Result<()> {
    match mkdirat(rootfd, VM_HOST_DIR, Mode::from_raw_mode(0o755)) {
        Ok(()) => Ok(()),
        Err(Errno::EXIST) => Err(err(format!("/{VM_HOST_DIR} already exists in the container rootfs"))),
        Err(e) => Err(os(|| format!("create /{VM_HOST_DIR}"), e)),
    }
}

fn attach(rootfd: BorrowedFd, fs: &Filesystem, clone: BorrowedFd) -> io::Result<()> {
    let dest = open_directory_at(rootfd, &format!("{VM_HOST_DIR}/{}", fs.target), true)
        .map_err(|e| err(format!("prepare /{VM_HOST_DIR}/{}: {e}", fs.target)))?;
    move_mount(
        clone,
        "",
        dest.as_fd(),
        "",
        MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH | MoveMountFlags::MOVE_MOUNT_T_EMPTY_PATH,
    )
    .map_err(|e| os(|| format!("attach /{VM_HOST_DIR}/{}", fs.target), e))
}

/// Publishes success. Written last and only once, because kata-agent ignores
/// hook failures: the marker is the only signal consumers can trust, so its
/// bytes are synced to surface any deferred write/close error here.
fn write_marker(rootfd: BorrowedFd, id: &str) -> io::Result<()> {
    let fd = openat(
        rootfd,
        READY_MARKER,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .map_err(|e| os(|| format!("create /{READY_MARKER}"), e))?;
    let mut f = fs::File::from(fd);
    f.write_all(format!("{id}\n").as_bytes())
        .map_err(|e| io::Error::new(e.kind(), format!("write /{READY_MARKER}: {e}")))?;
    f.sync_all()
        .map_err(|e| io::Error::new(e.kind(), format!("sync /{READY_MARKER}: {e}")))
}

/// Walks `path` below `parent` one component at a time, refusing symlinks, "."
/// and "..", and optionally creating missing directories.
fn open_directory_at(parent: BorrowedFd, path: &str, create: bool) -> io::Result<OwnedFd> {
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" => continue,
            "." | ".." => return Err(err(format!("path {path:?} has a relative component"))),
            p => parts.push(p),
        }
    }
    if parts.is_empty() {
        return Err(err(format!("empty path {path:?}")));
    }
    let mut current: Option<OwnedFd> = None;
    for part in parts {
        let dir = current.as_ref().map_or(parent, |f| f.as_fd());
        if create {
            // mkdirat never follows a symlink in the last component; the
            // O_NOFOLLOW open below then rejects a pre-existing one.
            match mkdirat(dir, part, Mode::from_raw_mode(0o755)) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(e) => return Err(os(|| format!("mkdir {part}"), e)),
            }
        }
        let next = openat(
            dir,
            part,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| os(|| format!("open {part}"), e))?;
        current = Some(next);
    }
    Ok(current.expect("parts is non-empty"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
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
            let p = std::env::temp_dir().join(format!("ddgkm-mnt-{}-{}", std::process::id(), n));
            std::fs::create_dir_all(&p).unwrap();
            TmpDir { path: std::fs::canonicalize(&p).unwrap() }
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    // A directory handle usable as the `parent` argument; keeps the fd open.
    fn dir(path: &Path) -> fs::File {
        fs::File::open(path).unwrap()
    }

    #[test]
    fn open_directory_at_walks_and_creates() {
        let root = TmpDir::new();
        let parent = dir(&root.path);
        open_directory_at(parent.as_fd(), "vm-host/sys/fs/cgroup", true).unwrap();
        assert!(root.path.join("vm-host/sys/fs/cgroup").is_dir());
    }

    #[test]
    fn open_directory_at_refuses_symlinks_and_traversal() {
        let root = TmpDir::new();
        let outside = TmpDir::new();
        std::fs::create_dir_all(outside.path.join("rootfs")).unwrap();
        symlink(outside.path.join("rootfs"), root.path.join("link")).unwrap();
        for path in ["link", "link/x", "..", "a/../b", "."] {
            for create in [false, true] {
                let parent = dir(&root.path);
                if open_directory_at(parent.as_fd(), path, create).is_ok() {
                    panic!("accepted {path:?} (create={create})");
                }
            }
        }
    }

    #[test]
    fn reserve_vm_host_refuses_existing() {
        let root = TmpDir::new();
        let parent = dir(&root.path);
        reserve_vm_host(parent.as_fd()).unwrap();
        // A second run, or an image that ships /vm-host, must not stack mounts.
        assert!(reserve_vm_host(parent.as_fd()).is_err(), "stacked on existing /vm-host");
    }

    #[test]
    fn write_marker_writes_once_and_refuses_symlink() {
        let root = TmpDir::new();
        std::fs::create_dir(root.path.join("vm-host")).unwrap();
        let parent = dir(&root.path);
        write_marker(parent.as_fd(), TEST_ID).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.path.join("vm-host/.ready-v1")).unwrap(),
            format!("{TEST_ID}\n")
        );
        assert!(write_marker(parent.as_fd(), TEST_ID).is_err(), "overwrote the marker");

        let other = TmpDir::new();
        std::fs::create_dir(other.path.join("vm-host")).unwrap();
        let victim = other.path.join("victim");
        symlink(&victim, other.path.join("vm-host/.ready-v1")).unwrap();
        let parent2 = dir(&other.path);
        assert!(write_marker(parent2.as_fd(), TEST_ID).is_err(), "wrote through a symlink");
        assert!(!victim.exists(), "created the symlink target");
    }

    // A procfs look-alike whose ns entries are plain symlinks, so namespace
    // identity can be checked without privileges.
    fn fake_proc() -> TmpDir {
        let p = TmpDir::new();
        for pid in ["1", "self", "4242"] {
            let d = p.path.join(pid).join("ns");
            std::fs::create_dir_all(&d).unwrap();
            for kind in ["pid", "cgroup", "user", "mnt"] {
                symlink(format!("{kind}:[1]"), d.join(kind)).unwrap();
            }
        }
        // The target must have its own mount namespace.
        let mnt = p.path.join("4242/ns/mnt");
        std::fs::remove_file(&mnt).unwrap();
        symlink("mnt:[2]", &mnt).unwrap();
        p
    }

    #[test]
    fn check_namespaces_accepts_initial_and_rejects_shared_mnt() {
        let p = fake_proc();
        let root = p.path.to_str().unwrap();
        check_namespaces(root, 4242).unwrap();
        // A target sharing the hook's mount namespace is refused.
        let mnt = p.path.join("4242/ns/mnt");
        std::fs::remove_file(&mnt).unwrap();
        symlink("mnt:[1]", &mnt).unwrap();
        assert!(check_namespaces(root, 4242).is_err());
    }
}
