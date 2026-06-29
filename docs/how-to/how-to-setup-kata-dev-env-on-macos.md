# Set up a Kata Containers dev environment on macOS

This guide walks through setting up a local Kata Containers development environment
on Apple Silicon Macs (M3 and later). The environment runs inside a [Lima] VM with
nested KVM, so real Kata micro-VMs (QEMU + KVM) run entirely on your Mac without
any remote Linux machine.

## Requirements

- Apple Silicon Mac (M3 or later) — Lima's `vmType: vz` with
  `nestedVirtualization: true` requires the Apple Virtualization framework,
  which exposes nested virtualization on M3 and later chips.
- macOS 15 (Sequoia) or later — required for nested virtualization.
- ~80 GiB free disk — the VM requests an 80 GiB disk image plus a `cache/`
  directory for downloaded artifacts (~700 MB on first run).

[Apple documents nested virtualization support](https://developer.apple.com/documentation/virtualization/vzgenericplatformconfiguration/isnestedvirtualizationsupported)
as starting with M3, and [Lima documents M3 and macOS 15 or later](https://github.com/lima-vm/lima/blob/master/templates/default.yaml)
for its `vz` backend. These requirements apply to the host virtualization stack
used here: macOS runs the Lima Linux VM, which must expose KVM so Kata can run
its own guest VMs.

## Prerequisites

Install [Lima]:

```sh
brew install lima
```

## Quick start

Scripts live in `tools/packaging/dev-setup/macos/` inside the kata-containers
repository. Run them from that directory:

```sh
cd tools/packaging/dev-setup/macos

./setup.sh          # Create VM, install Kata + k3s, run smoke test (~10 min on first run)
./verify.sh         # Re-run checks without recreating the VM
./teardown.sh       # Stop and delete the VM
```

The baseline uses the upstream Kata Containers Go-runtime archive
(`kata-go-static`) pinned in `setup.sh` `KATA_VERSION`, independently of
local Git tags.

`setup.sh` is idempotent — re-running it on an already-provisioned VM skips
provisioning and jumps directly to verification.

k3s is installed by default. To skip it:

```sh
./setup.sh --no-k3s
```

To install or remove k3s separately after the VM is already provisioned:

```sh
./install-k3s.sh     # Install k3s + kata RuntimeClass, export kubeconfig
./uninstall-k3s.sh   # Remove k3s (leaves VM and Kata intact)
```

### Optional: build components from source

To overlay locally-built binaries onto the baseline `/opt/kata` install:

```sh
./setup.sh --build                   # fast set: agent, shim-v2, runtime
./setup.sh --build=shim-v2,runtime   # specific components only
```

This compiles Go and Rust components from the virtiofs-mounted source tree inside
the Lima VM.

Supported components: `shim-v2`, `runtime`, `agent`. The shim and runtime binaries
are installed into `/opt/kata`. The agent is compiled with the VM's native GNU
toolchain; using a custom agent inside Kata also requires rebuilding the guest
rootfs.

Kernel and QEMU rebuilds are slow and require Docker inside the VM. Use the
`tools/packaging/kata-deploy/local-build/` Makefile targets described in
[Building and Deploying Local Kata Artifacts](how-to-build-and-deploy-local-artifacts.md).

## Architecture

```
macOS (Apple Hypervisor.framework)
└── Lima VM  Ubuntu arm64  (vmType: vz, nestedVirtualization: true)
    ├── KVM (nested via Apple vz)
    │   └── Kata micro-VMs (QEMU + KVM)
    ├── system containerd  → nerdctl smoke tests
    ├── k3s (optional)     → RuntimeClass kata
    └── virtiofs           → macOS $HOME (writable, same path in VM)
```

Lima's virtiofs mount maps your macOS `$HOME` into the VM at the same path.
This makes kata-containers checkouts on macOS available inside the VM.

## Development workflow

Open a shell inside the VM:

```sh
limactl shell kata-dev
```

Build and install individual components with `make` (Go) or `cargo build` (Rust):

```sh
# Build and install the runtime shim
cd ~/path/to/kata-containers/src/runtime
make -B PREFIX=/opt/kata containerd-shim-v2
sudo install -m755 containerd-shim-kata-v2 /opt/kata/bin/
sudo ln -sf /opt/kata/bin/containerd-shim-kata-v2 /usr/local/bin/containerd-shim-kata-v2
sudo systemctl restart containerd

# Run a test container — output should be the guest kernel, not the host kernel
sudo nerdctl run --rm --runtime io.containerd.kata.v2 alpine uname -r
```

### Kubernetes workflow (with k3s)

```sh
# Apply the example pod
kubectl apply -f tools/packaging/dev-setup/macos/examples/pod.yaml
kubectl wait pod/kata-test --for=jsonpath='{.status.phase}'=Succeeded --timeout=120s
kubectl logs kata-test   # prints the kata guest kernel version

# After rebuilding the shim, restart k3s's containerd to pick it up
sudo systemctl restart k3s
```

### Key paths inside the VM

| Path                                                          | Contents                                                          |
| ------------------------------------------------------------- | ----------------------------------------------------------------- |
| `/opt/kata/bin/`                                              | `kata-runtime`, `containerd-shim-kata-v2`, QEMU, cloud-hypervisor |
| `/opt/kata/share/kata-containers/`                            | Guest kernels and rootfs images                                   |
| `/opt/kata/share/defaults/kata-containers/configuration.toml` | Active Kata config (QEMU)                                         |
| `/etc/containerd/config.toml`                                 | containerd config with kata runtime entry                         |

## Known limitations

### PMU=off on Apple Hypervisor.framework (nested KVM)

The Kata static release includes `cpu_features = "pmu=off"` in
`configuration.toml`. Apple's Hypervisor.framework does not expose the
`host-arm-cpu.pmu` property to nested guests, so QEMU treats `pmu=off` as a
fatal error and exits — causing Kata containers to fail on startup with no
diagnostic output.

`provision.sh` clears `pmu=off` automatically after installing the baseline
tarball, and `build.sh` re-applies the fix after any build overlay. If you
manually regenerate the configuration from source, apply the fix yourself:

```sh
sudo sed -i 's/^cpu_features = "pmu=off"/cpu_features = ""/' \
    /opt/kata/share/defaults/kata-containers/configuration.toml \
    /opt/kata/share/defaults/kata-containers/configuration-qemu.toml
```

This limitation is specific to Lima + vz + Apple Silicon nested KVM.

### Guest connection timeouts after repeated runs

If the smoke tests time out connecting to the guest over vsock, including with
an unmodified release shim, restart the Lima VM and rerun verification:

```sh
limactl stop kata-dev
./setup.sh
```

## Teardown

```sh
./teardown.sh    # Stop and delete the Lima VM
```

The exported `kubeconfig.yaml` is removed. The `cache/` directory is preserved
on teardown so a subsequent `./setup.sh` reuses already-downloaded artifacts.

[Lima]: https://lima-vm.io
