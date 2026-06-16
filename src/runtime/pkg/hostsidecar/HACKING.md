# hostsidecar — maintainer / rebase notes

`pkg/hostsidecar` implements **host-side sidecar containers** for Kata: a
container annotated as a host sidecar runs via an OCI runtime (`runc`) on the
host, in the pod network namespace, instead of inside the guest VM. See
`Terrapin/Host-side Kata sidecars/06-proposal-a-implementation-plan.md` in the
design repo for the full rationale.

This is a **downstream fork feature**. It is built as a clean overlay on
upstream so it carries forward across rebases with minimal conflict.

## Design constraint: minimise the upstream diff

All feature logic lives in this self-contained package
(`src/runtime/pkg/hostsidecar/`), which does not exist upstream and therefore
cannot conflict on a rebase. The only edits to files that exist in
**`upstream` (`kata-containers/kata-containers`)** are a handful of thin
delegation lines, listed below. The integration *glue* (exit reaping, IO
wiring) lives in a new file, `pkg/containerd-shim-v2/hostsidecar_shim.go`,
which is also conflict-free.

## Upstream insertion points

Re-apply these after a rebase onto a new upstream release. Each is a small,
mechanical edit.

| File | Edit | Why |
|------|------|-----|
| `pkg/containerd-shim-v2/service.go` | add field `hostMgr *hostsidecar.Manager` to `service`; initialise it in `New()` | the shim owns one manager per pod |
| `pkg/containerd-shim-v2/container.go` | add field `host *hostsidecar.Container` to `container` | marks a shim container as host-backed |
| `pkg/containerd-shim-v2/create.go` | in the `PodContainer` case, before `katautils.CreateContainer`, branch: if `s.hostMgr.Enabled() && hostsidecar.IsHostSidecar(ociSpec)` route to the host (`startHostContainer` glue) instead of the VM | the routing decision |
| `pkg/containerd-shim-v2/{service.go}` | one-line guard at the head of `Start/Delete/Kill/State/Exec/ResizePty/Pause/Resume/Stats/Update`: `if c.host != nil { return <glue>.<op>(...) }` | delegate lifecycle to runc for host containers |

Everything those edits call is in `hostsidecar` or in the new
`hostsidecar_shim.go` glue file.

## Verified facts (so the wiring is grounded, not assumed)

- **Pod netns path**: obtained from `s.sandbox.GetNetNs()` (interface method on
  `vc.VCSandbox`, `virtcontainers/interfaces.go`). The existing PodContainer
  path already uses it for prestart/poststart hooks
  (`pkg/katautils/create.go`, `start.go`), so it is reliably populated by the
  time a `PodContainer` is created. We do **not** rely on the netns being in
  the container OCI spec.
- **runc**: `github.com/containerd/go-runc` is already in the module graph
  (`v1.1.0`); no new dependency is required. `*runc.Runc` satisfies the
  package's `OCIRuntime` interface directly.
- **cgroups**: the OCI runtime creates the sidecar cgroup from the bundle's
  `linux.cgroupsPath` (set by `rewriteSpecForHost` to `/kata/<sandbox>/<id>`),
  so no separate cgroup library is needed.

## Exit reaping (the one subtle part)

A host sidecar's init process is created detached, so it is **not** a child of
the shim and cannot be `waitpid`-ed directly. Installing a SIGCHLD/subreaper
handler in the kata shim is rejected: the shim already reaps the hypervisor
(`s.hpid`), and a process-wide reaper would collide with that.

Instead, `Container.watch()` (in `container.go`) polls the OCI runtime state on
a short interval and treats `stopped` (or a vanished state) as exit. On exit it
records status/code/time exactly once and invokes the `OnExit` callback the
shim supplied in `CreateParams`. The glue file's `OnExit` feeds the shim
container's `exitCh` and emits a `TaskExit` event — mirroring `wait()` on the
in-VM path (`start.go`). Exit code is best-effort: inferred from the last
delivered signal, since runc does not report the real code to a non-parent.
The polling FSM is unit-tested with a scripted fake runtime; end-to-end exit
behaviour is validated by the e2e harness (plan milestone M4).

## Tests

- Unit (no root, runs anywhere): `go test ./pkg/hostsidecar/...`. Covers
  annotation parsing, spec rewrite, bundle write, and the manager/container FSM
  against a fake `OCIRuntime`.
- Integration / e2e (in the Lima VM, on k3s): validates real runc lifecycle,
  netns placement, `kubectl exec`/`logs`, and exit reaping. See the design
  repo's M4 harness.

## tapnet networking mode

`internetworking_model=tapnet` is a new variant of the `none` model where
the host-sidecar proxy drives a gVisor user-space TCP/IP stack instead of
using iptables REDIRECT.

### Architecture

```
  Kata shim                  QEMU
  setupTapnetNetworking()    -netdev stream,server=on,
  → MkdirAll /run/kata-tapnet  addr.type=unix,
  → sets VM IP/routes          addr.path=/run/kata-tapnet/tap0_kata.sock
  → no tap device, no VMFds                  |
                                             | Unix stream socket
                                             | (4-byte BE length-prefix frames)
  kata-dev-network-proxy                     |
  --tapnet-socket=/run/kata-tapnet/tap0_kata.sock
  → dials socket  ←——————————————————————————┘
  → AcceptQemu(ctx, conn)
  → gVisor handles all VM frames in user space
  → TCP/UDP NATted via host eth0
```

QEMU acts as the **server** (`server=on`) and listens on the socket before
the proxy container starts.  The proxy **dials** the socket (with a 15 s
retry window) and calls `AcceptQemu`, which speaks QEMU's 4-byte big-endian
length-prefix framing (same as `gvproxy` in Podman Machine).

### QEMU version requirement

The `stream` netdev type is available from **QEMU 7.2+**.  The kata-static
binary in the dev VM is QEMU 10.2.1 — well within range.

Verify:
```sh
/opt/kata/bin/qemu-system-aarch64 -machine virt -netdev help 2>&1 | grep stream
```

### Socket path convention

The shim derives the socket path from `netPair.TAPIface.Name` (e.g. `tap0_kata`):

```
/run/kata-tapnet/<tap-name>.sock
```

This is created by QEMU at sandbox start.  `removeTapnetNetworking` removes it
on sandbox teardown.

The proxy container needs `/run/kata-tapnet/` mounted as a hostPath volume
so it can reach the socket created by QEMU (which runs on the host).

### Upstream insertion points (tapnet additions)

| File | Edit |
|------|------|
| `pkg/govmm/qemu/qemu.go` | `NetDeviceStream` type, `SocketPath` field, stream case in `QemuNetdevParam`/`QemuDeviceParam`/`QemuNetdevParams` |
| `virtcontainers/network.go` | `NetXConnectNoneTapnetModel` iota value, `"tapnet"` string |
| `virtcontainers/network_linux.go` | `tapnetSocketDir`, `tapnetSocketPath`, `setupTapnetNetworking`, `removeTapnetNetworking` |
| `virtcontainers/qemu_arch_base.go` | `networkModelToQemuType` and `genericNetwork` tapnet branches |
| `pkg/katautils/config.go` | `checkNetNsConfig` accepts `tapnet` alongside `none` |

### Reproducing the setup from scratch

1. Set `internetworking_model = tapnet` in configuration.toml (the
   `apply-config.sh` script does this by default).
2. Build and deploy the shim (`deploy.sh`).
3. Build and load the proxy image (`build-load-proxy.sh`).
4. Apply `proxy-demo.yaml` (uses `--tapnet-socket` and a hostPath volume).
5. Verify:

   ```sh
   # QEMU socket exists after pod start:
   ls -la /run/kata-tapnet/

   # No iptables rules installed:
   ip netns exec <cni-ns> iptables -t nat -L PREROUTING

   # VM reaches external HTTP via gVisor NAT:
   k3s kubectl exec proxy-demo -c workload -- wget -qO- http://example.com
   ```
