# How to use host-side-proxy networking (`jailnet` and `tapnet`)

This document covers two `internetworking_model` options built for a specific use case:
routing **all** VM network traffic through a proxy container running alongside the Kata
sandbox, instead of Kata's normal container-network path (`tcfilter`/`macvtap`, which simply
bridge the pod's own network interface into the VM).

They are two independent, selectable implementations of the same goal — pick one, not both:

| | `jailnet` | `tapnet` |
|---|---|---|
| Mechanism | Kernel tap device in an isolated "jail" netns, bridged to the pod netns via a veth pair, with iptables `REDIRECT` rules | VM's virtio-net device backed directly by a Unix socketpair; no kernel tap device at all |
| What reaches the proxy | Only TCP and UDP/53 (DNS) — other protocols are dropped by the pod netns's default `FORWARD` policy, not proxied | Everything — the proxy's user-space network stack sees every VM frame (TCP/UDP/ICMP) |
| Kernel involvement | Kernel forwards/redirects VM frames up to the interception point (tap + veth are real kernel netdevs — `vhost-net` and multiqueue apply) | None — the kernel never sees a VM frame; QEMU's `-netdev socket,fd=N` reads/writes the socketpair directly |
| Performance profile | Kernel-level throughput characteristics | User-space stack overhead; the socketpair backend is always single-queue (`mq` is suppressed) |
| Guarantee | Fails closed for unhandled protocols (dropped) | No protocol is silently dropped, since the proxy sees all of it |

If you need maximum protocol coverage and a hard "no bypass" guarantee, use `tapnet`. If you
need the performance characteristics of a real kernel tap device and can tolerate only
TCP/DNS being interceptable, use `jailnet`.

Both share the same VM-side addressing: the VM is configured with `169.254.1.2/30` and a
default route via `169.254.1.1`, where the host-side proxy acts as gateway
(`proxyTapVMCIDR`/`proxyTapHostCIDR` in
`src/runtime/virtcontainers/network_linux.go`).

## Configuring either model

Set `internetworking_model` under `[hypervisor.qemu]` in your `configuration.toml`
(QEMU-only — this is not implemented for other hypervisor backends):

```toml
[hypervisor.qemu]
internetworking_model = "tapnet"   # or "jailnet"
```

Or per-pod via the OCI/CRI annotation (see
[how-to-set-sandbox-config-kata.md](how-to-set-sandbox-config-kata.md)):

```yaml
annotations:
  io.katacontainers.config.hypervisor.internetworking_model: "tapnet"
```

Both models also support `disable_new_netns = true` (the runtime accepts this only for
`jailnet`/`tapnet` — see `checkNetNsConfig` in `src/runtime/pkg/katautils/config.go`).

In both cases, **a proxy container or process must exist and be reachable from the pod
before the VM's network traffic is expected to flow.** Kata itself does not run the proxy —
that's the responsibility of whatever deploys the sandbox (e.g. a sidecar container or
DaemonSet-installed component).

## `jailnet`: jail netns + veth + iptables REDIRECT

### Topology

```
jail netns:  tap0_kata (169.254.1.1/30) ─── kata-pvj (169.254.2.1/30)
                                                  │ veth pair
pod  netns:                               kata-pvp (169.254.2.2/30) ─── eth0
```

`setupJailNetNetworking` (in `network_linux.go`) creates a network namespace named
`kata-jail-<id>` (bind-mounted at `/run/netns/kata-jail-<id>`) containing only the VM's tap
device, then bridges it to the pod netns with a veth pair (`kata-pvj` inside the jail,
`kata-pvp` in the pod netns). `<id>` is the sandbox's per-interface UUID
(`netPair.ID`), not a predictable name — see [Discovery](#discovery) below for why this
matters and its current limitation.

### What the proxy must do

The proxy runs **inside the pod netns** (i.e. as a container in the same pod, or otherwise
joined to that network namespace) and is responsible for:

1. Installing iptables `REDIRECT` rules on `kata-pvp` so that TCP and UDP/53 traffic
   arriving from the VM is redirected to a local listening port instead of reaching `eth0`.
2. Accepting the redirected connections and recovering the original destination via
   `SO_ORIGINAL_DST`, then forwarding to that destination via `eth0`.
3. Ensuring the pod netns's default `FORWARD` policy is `DROP` — this is what makes
   "everything else is dropped" a real guarantee rather than an assumption; Kata's shim code
   does not set this policy itself.

Sketch of the proxy-side setup (illustrative — an iptables library such as
[`coreos/go-iptables`](https://github.com/coreos/go-iptables) is a reasonable choice in Go):

```go
const (
    kataPvp      = "kata-pvp"
    proxyPort    = 15001 // wherever your proxy listens
)

func installRedirect(ipt *iptables.IPTables) error {
    if err := ipt.NewChain("nat", "KATA_PROXY"); err != nil {
        return err
    }
    // Redirect TCP from the VM (arriving on kata-pvp) to the local proxy port.
    if err := ipt.AppendUnique("nat", "PREROUTING",
        "-i", kataPvp, "-p", "tcp", "-j", "REDIRECT", "--to-port", fmt.Sprint(proxyPort)); err != nil {
        return err
    }
    // Redirect DNS (UDP/53) the same way.
    return ipt.AppendUnique("nat", "PREROUTING",
        "-i", kataPvp, "-p", "udp", "--dport", "53", "-j", "REDIRECT", "--to-port", fmt.Sprint(proxyPort))
}

func acceptRedirected(lc net.ListenConfig, ctx context.Context) error {
    ln, err := lc.Listen(ctx, "tcp", fmt.Sprintf(":%d", proxyPort))
    if err != nil {
        return err
    }
    for {
        conn, err := ln.Accept()
        if err != nil {
            return err
        }
        go func(c net.Conn) {
            defer c.Close()
            tcpConn := c.(*net.TCPConn)
            origDst, err := getOriginalDst(tcpConn) // SYS_GETSOCKOPT + SO_ORIGINAL_DST
            if err != nil {
                return
            }
            upstream, err := net.Dial("tcp", origDst.String())
            if err != nil {
                return
            }
            defer upstream.Close()
            go io.Copy(upstream, c)
            io.Copy(c, upstream)
        }(conn)
    }
}
```

## `tapnet`: socketpair + user-space stack

### How the handoff works

`setupTapnetNetworking` creates a Unix `socketpair(AF_UNIX, SOCK_STREAM)`. One end
(`fds[0]`) is passed to QEMU as `-netdev socket,fd=N` — this makes the virtio-net device's
carrier come up immediately at VM boot, with no dependency on the proxy being ready yet
(this is what eliminates the boot-race a naive "QEMU listens, proxy dials" design would
have). The other end (`fds[1]`, the "control" fd) is held by the shim, which:

1. Starts draining it in the background (`drainFile`) so VM TX frames don't fill the
   socketpair buffer while waiting for a proxy.
2. Listens on a Unix control socket at `/run/kata-tapnet/<id>.ctrl` (`<id>` again being
   `netPair.ID`, a per-interface UUID — see [Discovery](#discovery)).
3. On the first connection to that socket, sends the control fd to the connecting process
   via `SCM_RIGHTS` (`serveTapnetCtrl` in `network_linux.go`), then exits.

From that point on, the proxy owns the control fd and every byte written to/read from it is
a raw Ethernet frame from/to the VM's virtio-net device. **Reconnection is not supported**:
if the proxy exits, QEMU marks the virtio-net link down; a pod restart is required to
re-establish the socketpair.

### Proxy-side sample code

```go
// connectToTapnet dials the shim's control socket and receives the VM-facing
// fd via SCM_RIGHTS, returning it as an *os.File wrapping raw VM frames.
func connectToTapnet(ctrlPath string) (*os.File, error) {
    conn, err := net.Dial("unix", ctrlPath)
    if err != nil {
        return nil, fmt.Errorf("dial tapnet ctrl socket: %w", err)
    }
    defer conn.Close()

    uc := conn.(*net.UnixConn)
    rawConn, err := uc.SyscallConn()
    if err != nil {
        return nil, err
    }

    buf := make([]byte, 1)
    oob := make([]byte, unix.CmsgSpace(4)) // room for one fd
    var n, oobn int
    var recvErr error
    if err := rawConn.Read(func(fd uintptr) bool {
        n, oobn, _, _, recvErr = unix.Recvmsg(int(fd), buf, oob, 0)
        return true
    }); err != nil {
        return nil, err
    }
    if recvErr != nil {
        return nil, fmt.Errorf("recvmsg: %w", recvErr)
    }
    scms, err := unix.ParseSocketControlMessage(oob[:oobn])
    if err != nil || len(scms) == 0 {
        return nil, fmt.Errorf("no control message received (n=%d)", n)
    }
    fds, err := unix.ParseUnixRights(&scms[0])
    if err != nil || len(fds) == 0 {
        return nil, fmt.Errorf("no fd in control message")
    }
    return os.NewFile(uintptr(fds[0]), "tapnet-vm"), nil
}
```

Once you have the `*os.File`, wrap it as a `net.Conn` (or hand its fd to a packet-level
library) and drive a user-space TCP/IP stack against it — e.g.
[gVisor's netstack](https://github.com/google/gvisor/tree/master/pkg/tcpip), as used by
[gvisor-tap-vsock](https://github.com/containers/gvisor-tap-vsock). The frames you read are
raw Ethernet frames sent by the guest's virtio-net driver; whatever you write back is
delivered to the guest the same way.

## Discovery

The `<id>` component of both the jail netns name (`kata-jail-<id>`) and the tapnet control
socket path (`/run/kata-tapnet/<id>.ctrl`) is `<sandbox ID>-<interface index>` — set in
`addSingleEndpoint` from the sandbox's CRI/OCI ID (the same one visible via `crictl pods`),
not a randomly generated value. This keeps the name globally unique (fixing an earlier bug
where both were keyed on the per-sandbox-but-not-globally-unique tap interface name, e.g.
`tap0_kata`, which collided between sandboxes) while staying computable by anything that
already knows the pod's sandbox ID — in particular, whatever orchestrates the proxy sidecar
can pass it the sandbox ID (e.g. via the Kubernetes downward API) and it can compute
`/run/kata-tapnet/<sandboxID>-0.ctrl` directly, without needing to glob the directory.

`-0` is the index of the pod's first (and for a typical single-NIC pod, only) network
interface; a second interface on the same sandbox would be `-1`, and so on, matching the
order interfaces are attached in.

## Limitations

- `jailnet` only proxies TCP and UDP/53; every other protocol is dropped, not forwarded.
- `tapnet` does not support proxy reconnection — a proxy crash or restart requires the pod
  to be restarted to get a new socketpair.
- Both require an external proxy component; neither is useful on its own.
- Only implemented for the QEMU hypervisor backend.
