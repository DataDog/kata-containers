# Guest kernel mounts hook (`dd-guest-kernel-mounts`)

A Kata **guest** OCI prestart hook, built into the Datadog guest image at
`/usr/share/datadog/kata-guest-hooks/prestart/10-guest-kernel-mounts`.
kata-agent runs it for every container when the runtime configuration sets
`guest_hook_path = "/usr/share/datadog/kata-guest-hooks"` (see
`docker/containerd/config.d/10-override.toml`).

## Why

CI jobs that run the Datadog Agent with CWS inside a GitLab docker-in-docker
service cannot see the kernel. `--pid=host` there only reaches the
docker-in-docker PID namespace, and `debugfs`, `tracefs` and `securityfs` are
not available. The hook gives an explicitly opted-in docker-in-docker
container a view of the guest's own kernel filesystems. Nothing from the
Kubernetes node is exposed: all sources belong to the microVM guest.

## Contract (v1)

kata-agent writes the OCI runtime state to the hook's stdin (the OCI hook
contract). The hook acts only when **all** of these annotations are present on
that state:

| Annotation | Value |
|---|---|
| `io.kubernetes.cri.container-type` | `container` |
| `io.kubernetes.cri.container-name` | `docker` (the runner's docker-in-docker service) |
| `io.katacontainers.datadog.guest-kernel-mounts` | `v1` |

The last one is a pod annotation, set from a GitLab
`KUBERNETES_POD_ANNOTATIONS_*` job variable that the runner configuration must
allow. containerd forwards it because the Kata handlers allow
`io.katacontainers.*` pod annotations. Any other container exits 0 without
touching the filesystem. If the target container carries any other value, the
hook fails.

It then exposes, inside the container rootfs:

| Guest source | Container path | Mode |
|---|---|---|
| `/proc` (guest-initial PID namespace) | `/vm-host/proc` | `ro,nosuid,nodev,noexec` |
| `/sys/fs/cgroup` (root cgroup view) | `/vm-host/sys/fs/cgroup` | `ro,nosuid,nodev,noexec` |
| `/sys/kernel/debug` | `/vm-host/sys/kernel/debug` | `rw,nosuid,nodev,noexec` |
| `/sys/kernel/tracing` | `/vm-host/sys/kernel/tracing` | `rw,nosuid,nodev,noexec` |
| `/sys/kernel/security` | `/vm-host/sys/kernel/security` | `rw,nosuid,nodev,noexec` |

`/vm-host/.ready-v1`, containing the container ID, is created last.
**kata-agent ignores hook failures**, so the marker is the only success
signal. A `/vm-host` without the marker means "not ready".

## Behaviour

1. Decode the state (at most 1 MiB) and apply the gate above.
2. Require `status == "created"`, `pid > 1`, a plain container ID, and
   `bundle == /run/kata-containers/<id>` without symlink components. The rootfs
   is `<bundle>/rootfs` by kata-agent's convention, so it is derived rather than
   read back from the bundle's `config.json`.
3. Require that the hook runs in PID 1's PID, cgroup and user namespaces, that
   the target is not user-namespace remapped, and that the target has its own
   mount namespace.
4. Mount `debugfs`, `tracefs` and `securityfs` at their usual guest paths if
   they are not mounted yet. Check every source's filesystem type.
5. Clone each source with `open_tree(OPEN_TREE_CLONE)` (non-recursive) and set
   its attributes with `mount_setattr`.
6. Enter the target's mount namespace. Before `pivot_root`, walk `<bundle>/rootfs`
   from the task's root, one component at a time and without following symlinks.
   Create `/vm-host` (it must not exist), attach each clone with `move_mount`,
   then write the marker.

The binary takes no flags. Failures are reported on stderr, which kata-agent
copies to its own log, and in the guest kernel log, which is the easier place
to look:

```sh
dmesg | grep -E 'kata-agent: hook failed|dd-guest-kernel-mounts'
```

## Security notes

- The opted-in docker-in-docker container already runs privileged inside the
  guest. The hook additionally shows it the guest-initial process tree
  (including the guest system-probe) and the root cgroup view. Only pods
  admitted with the annotation get this, and the runner allowlist is the policy.
- `cgroup.procs` and `/proc/<pid>/cgroup` are relative to the reader's
  namespaces, so an Agent using these mounts cannot fully resolve cgroups.
  Never enable CWS enforcement in this topology.

## Development

Rust, built for `linux/amd64` and `linux/arm64` only. The toolchain is pinned in
`rust-toolchain.toml` (kept in sync with `versions.yaml`).

```sh
cargo test
cargo clippy --all-targets
cargo build --release --target x86_64-unknown-linux-musl
```

The tests run unprivileged and cover the gate, state validation, the
symlink/traversal-safe path walk, and the readiness marker. Cloning and
attaching mounts needs `CAP_SYS_ADMIN` in the guest-initial namespaces, so only
a real Kata sandbox exercises that path. `tools/osbuilder/rootfs-builder/rootfs.sh`
builds the static musl binary while it assembles the Datadog guest rootfs.
