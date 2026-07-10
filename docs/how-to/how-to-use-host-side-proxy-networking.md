# How to use host-side-proxy networking (`jailnet` and `tapnet`)

This document covers two `internetworking_model` options built for a specific use case:
routing **all** VM network traffic through a proxy container running alongside the Kata
sandbox, instead of Kata's normal container-network path (`tcfilter`/`macvtap`, which simply
bridge the pod's own network interface into the VM).

They are two independent, selectable implementations of the same goal — pick one, not both:

| | `jailnet` | `tapnet` |
|---|---|---|
| Mechanism | Kernel tap device in an isolated "jail" netns, bridged to the pod netns via a veth pair, with iptables `REDIRECT` rules | VM's virtio-net device backed directly by a Unix socketpair; no kernel tap device at all |
| What reaches the proxy | Only what the proxy's own `REDIRECT` rules match (typically TCP and UDP/53) — everything else is forwarded to `eth0` unmodified *unless the proxy also installs a `FORWARD` drop rule for it* (see [What the proxy must do](#what-the-proxy-must-do)) | Everything — the proxy's user-space network stack sees every VM frame (TCP/UDP/ICMP); there is no separate rule to remember |
| Kernel involvement | Kernel forwards/redirects VM frames up to the interception point (tap + veth are real kernel netdevs — `vhost-net` and multiqueue apply) | None — the kernel never sees a VM frame; QEMU's `-netdev socket,fd=N` reads/writes the socketpair directly |
| Performance profile | Kernel-level throughput characteristics | User-space stack overhead; the socketpair backend is always single-queue (`mq` is suppressed) |
| Isolation | Only as strong as the proxy's own iptables rules — Kata does not enforce or verify them | Structural: the kernel never sees a VM frame, so there is nothing for the proxy to forget to configure |

`tapnet` gives you protocol coverage and a bypass guarantee that holds regardless of the
proxy's own correctness, at the cost of user-space stack performance. `jailnet` gives you
kernel-level performance, but "no bypass" is not something Kata provides for you — it is
entirely the proxy's responsibility to get both the `REDIRECT` rules and the `FORWARD` drop
rule right (see below). Use `jailnet` if you need the performance and can be confident in
the proxy's own iptables setup; use `tapnet` if you want that guarantee to not depend on it.

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
`kata-pvp` in the pod netns). `<id>` is `netPair.ID` — see [Discovery](#discovery) below for
what it's derived from and why that matters.

### What the proxy must do

The proxy runs **inside the pod netns** (i.e. as a container in the same pod, or otherwise
joined to that network namespace) and is responsible for all of the following. The first
two are what makes traffic reach the proxy at all; the third is what makes everything else
actually get dropped rather than silently forwarded — **it is easy to build a proxy that
does the first two and skips the third, and it will look like it's working (TCP and DNS get
proxied) while every other protocol goes straight to `eth0` unfiltered.** Kata's shim does
not set this up for you: `setupJailNetNetworking` enables `ip_forward` inside the jail netns
purely so the kernel is capable of moving packets from the tap onto the veth — that is what
makes forwarding *possible*, and by itself it does nothing to restrict *what* gets forwarded.

1. Install iptables `REDIRECT` rules on `kata-pvp` so that TCP and UDP/53 traffic arriving
   from the VM is redirected to a local listening port instead of reaching `eth0`.
2. Accept the redirected TCP connections (and receive the redirected UDP/53 packets),
   recover the original destination, and forward accordingly.
3. **Install a rule that drops everything else reaching the `FORWARD` chain from
   `kata-pvp`.** `REDIRECT` only diverts what it matches — traffic that misses both rules
   above is not implicitly blocked, it is forwarded normally like any other packet in the
   netns unless something explicitly stops it. This rule is that something.

Full example (an iptables library such as
[`coreos/go-iptables`](https://github.com/coreos/go-iptables) is a reasonable choice in Go).
This is illustrative, not production code — for example, it copies raw UDP payloads without
DNS-aware parsing/validation, and error handling is trimmed for brevity:

```go
const (
    kataPvp      = "kata-pvp"
    tcpProxyPort = 15001 // wherever your TCP proxy listens
    dnsProxyPort = 15053 // wherever your DNS proxy listens
    upstreamDNS  = "8.8.8.8:53"
)

// installRedirect sets up both interception (steps 1-2) and enforcement
// (step 3). Do not skip the FORWARD rule at the end — without it this
// function only implements a transparent proxy, not an isolation boundary.
func installRedirect(ipt *iptables.IPTables) error {
    if err := ipt.AppendUnique("nat", "PREROUTING",
        "-i", kataPvp, "-p", "tcp", "-j", "REDIRECT", "--to-port", fmt.Sprint(tcpProxyPort)); err != nil {
        return err
    }
    if err := ipt.AppendUnique("nat", "PREROUTING",
        "-i", kataPvp, "-p", "udp", "--dport", "53", "-j", "REDIRECT", "--to-port", fmt.Sprint(dnsProxyPort)); err != nil {
        return err
    }
    // REDIRECTed packets are rewritten to a local destination before the
    // FORWARD chain is ever evaluated, so neither rule above interacts with
    // this one. Anything that reaches this rule is, by construction,
    // traffic that matched neither REDIRECT above — drop it here, or it
    // goes to eth0 exactly as if this proxy did not exist.
    return ipt.AppendUnique("filter", "FORWARD", "-i", kataPvp, "-j", "DROP")
}

// acceptTCP handles the REDIRECTed TCP traffic (installRedirect rule 1).
func acceptTCP(ctx context.Context, lc net.ListenConfig) error {
    ln, err := lc.Listen(ctx, "tcp", fmt.Sprintf(":%d", tcpProxyPort))
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
            origDst, err := getOriginalDst(c.(*net.TCPConn))
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

// soOriginalDst is SO_ORIGINAL_DST from linux/netfilter_ipv4.h — the only
// way to recover a REDIRECTed connection's pre-rewrite destination; without
// this, the proxy only ever sees its own listening address on tcpProxyPort.
const soOriginalDst = 80

func getOriginalDst(conn *net.TCPConn) (*net.TCPAddr, error) {
    rawConn, err := conn.SyscallConn()
    if err != nil {
        return nil, err
    }

    var addr syscall.RawSockaddrInet4
    addrLen := uint32(unsafe.Sizeof(addr))
    var sockErr error
    if err := rawConn.Control(func(fd uintptr) {
        _, _, errno := syscall.Syscall6(syscall.SYS_GETSOCKOPT, fd,
            syscall.IPPROTO_IP, soOriginalDst,
            uintptr(unsafe.Pointer(&addr)), uintptr(unsafe.Pointer(&addrLen)), 0)
        if errno != 0 {
            sockErr = errno
        }
    }); err != nil {
        return nil, err
    }
    if sockErr != nil {
        return nil, fmt.Errorf("getsockopt SO_ORIGINAL_DST: %w", sockErr)
    }

    ip := net.IPv4(addr.Addr[0], addr.Addr[1], addr.Addr[2], addr.Addr[3])
    port := int(addr.Port[0])<<8 | int(addr.Port[1]) // network byte order
    return &net.TCPAddr{IP: ip, Port: port}, nil
}

// handleDNS handles the REDIRECTed UDP/53 traffic (installRedirect rule 2).
// Unlike TCP, no SO_ORIGINAL_DST-style recovery is needed for the reply:
// REDIRECT's conntrack NAT state un-rewrites the source on the way back, so
// writing to whatever address the query came from is sufficient — the VM
// sees the reply as if it came from whatever it originally queried.
func handleDNS(ctx context.Context, lc net.ListenConfig) error {
    pc, err := lc.ListenPacket(ctx, "udp", fmt.Sprintf(":%d", dnsProxyPort))
    if err != nil {
        return err
    }
    defer pc.Close()

    buf := make([]byte, 4096)
    for {
        n, clientAddr, err := pc.ReadFrom(buf)
        if err != nil {
            return err
        }
        query := append([]byte(nil), buf[:n]...)
        go func(query []byte, clientAddr net.Addr) {
            upstream, err := net.Dial("udp", upstreamDNS)
            if err != nil {
                return
            }
            defer upstream.Close()
            upstream.SetDeadline(time.Now().Add(5 * time.Second))
            if _, err := upstream.Write(query); err != nil {
                return
            }
            resp := make([]byte, 4096)
            n, err := upstream.Read(resp)
            if err != nil {
                return
            }
            pc.WriteTo(resp[:n], clientAddr)
        }(query, clientAddr)
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
   `netPair.ID` — see [Discovery](#discovery)).
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

- `jailnet` only proxies whatever protocols the proxy's `REDIRECT` rules match (typically TCP
  and UDP/53), and only *drops* everything else if the proxy also installs the `FORWARD` drop
  rule described above — Kata does not do either of these for you, and a proxy that skips the
  drop rule will silently forward unmatched traffic instead of blocking it.
- `tapnet` does not support proxy reconnection — a proxy crash or restart requires the pod
  to be restarted to get a new socketpair.
- Both require an external proxy component; neither is useful on its own.
- Only implemented for the QEMU hypervisor backend.
