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
the shim and cannot be `waitpid`-ed directly. The containerd shim runtime sets
the shim as a child subreaper, so the reparented init becomes reapable by the
shim. The glue file must wire a go-runc process monitor / reaper so that, on
sidecar exit, it feeds the shim container's `exitCh` and emits a `TaskExit`
event — mirroring `wait()` on the in-VM path (`start.go`). This is integration
behaviour validated by the e2e harness (plan milestone M4), not by unit tests.

## Tests

- Unit (no root, runs anywhere): `go test ./pkg/hostsidecar/...`. Covers
  annotation parsing, spec rewrite, bundle write, and the manager/container FSM
  against a fake `OCIRuntime`.
- Integration / e2e (in the Lima VM, on k3s): validates real runc lifecycle,
  netns placement, `kubectl exec`/`logs`, and exit reaping. See the design
  repo's M4 harness.
