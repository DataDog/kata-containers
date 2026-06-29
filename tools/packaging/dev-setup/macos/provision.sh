#!/usr/bin/env bash
# Runs inside the kata-dev Lima VM over SSH (via limactl shell).
# Versions and CACHE_DIR are passed as env vars by setup.sh.
set -euo pipefail

export DEBIAN_FRONTEND=noninteractive

: "${GO_VERSION:?GO_VERSION must be set by setup.sh}"
: "${NERDCTL_VERSION:?NERDCTL_VERSION must be set by setup.sh}"
: "${KATA_VERSION:?KATA_VERSION must be set by setup.sh}"
: "${RUST_VERSION:?RUST_VERSION must be set by setup.sh}"
: "${CNI_PLUGINS_VERSION:?CNI_PLUGINS_VERSION must be set by setup.sh}"
CACHE_DIR="${CACHE_DIR:-}"

cached_or_download() {
    local filename="$1" url="$2"
    if [[ -n "${CACHE_DIR}" ]] && [[ -f "${CACHE_DIR}/${filename}" ]]; then
        echo "${CACHE_DIR}/${filename}"
    else
        wget -q "${url}" -O "/tmp/${filename}"
        echo "/tmp/${filename}"
    fi
}

# Apply the ARM64 nested-KVM PMU workaround.
# Apple Hypervisor.framework does not expose the `host-arm-cpu.pmu` property to
# nested guests. QEMU treats a missing PMU as a fatal error when `pmu=off` is set,
# causing every Kata container start to fail silently. Clearing the option makes
# QEMU accept the Apple vz nested-KVM environment.
# This function is idempotent — safe to call after both baseline installs and
# from-source build overlays (which regenerate configuration.toml from source).
# See docs/how-to/how-to-setup-kata-dev-env-on-macos.md "Known limitations".
apply_pmu_fix() {
    local toml
    for toml in \
        /opt/kata/share/defaults/kata-containers/configuration.toml \
        /opt/kata/share/defaults/kata-containers/configuration-qemu.toml; do
        [[ -f "${toml}" ]] || continue
        if grep -q 'pmu=off' "${toml}"; then
            sudo sed -i 's/^cpu_features = "pmu=off"/cpu_features = ""/' "${toml}"
        fi
    done
}

echo "==> Installing system packages..."
sudo apt-get update -y -q
sudo apt-get install -y -q \
    build-essential git curl wget pkg-config \
    libseccomp-dev protobuf-compiler libprotobuf-dev \
    musl-tools zstd

echo "==> Installing Go ${GO_VERSION}..."
if [[ ! -d /usr/local/go ]]; then
    src=$(cached_or_download "go${GO_VERSION}.linux-arm64.tar.gz" \
        "https://go.dev/dl/go${GO_VERSION}.linux-arm64.tar.gz")
    sudo tar -C /usr/local -xzf "${src}"
    [[ "${src}" = "/tmp/go${GO_VERSION}.linux-arm64.tar.gz" ]] && rm "${src}"
fi
# shellcheck disable=SC2016  # $PATH/$HOME must be written literally, not expanded
printf 'export PATH=$PATH:/usr/local/go/bin:$HOME/go/bin\n' \
    | sudo tee /etc/profile.d/go.sh > /dev/null

echo "==> Installing containerd..."
sudo apt-get install -y -q containerd
sudo mkdir -p /etc/containerd
containerd config default | sudo tee /etc/containerd/config.toml > /dev/null
sudo sed -i 's/^imports = .*/imports = ["\/etc\/containerd\/conf.d\/*.toml"]/' /etc/containerd/config.toml
sudo mkdir -p /etc/containerd/conf.d
# containerd 2 uses a different CRI plugin name from containerd 1.
CRI_PLUGIN="io.containerd.grpc.v1.cri"
if sudo grep -q 'io.containerd.cri.v1.runtime' /etc/containerd/config.toml; then
    CRI_PLUGIN="io.containerd.cri.v1.runtime"
fi
sudo tee /etc/containerd/conf.d/kata.toml > /dev/null <<EOF
[plugins.'${CRI_PLUGIN}'.containerd.runtimes.kata]
  runtime_type = 'io.containerd.kata.v2'
  [plugins.'${CRI_PLUGIN}'.containerd.runtimes.kata.options]
    ConfigPath = '/opt/kata/share/defaults/kata-containers/configuration.toml'
EOF
sudo systemctl enable --now containerd

echo "==> Installing nerdctl ${NERDCTL_VERSION}..."
src=$(cached_or_download "nerdctl-${NERDCTL_VERSION}-linux-arm64.tar.gz" \
    "https://github.com/containerd/nerdctl/releases/download/v${NERDCTL_VERSION}/nerdctl-${NERDCTL_VERSION}-linux-arm64.tar.gz")
sudo tar -C /usr/local/bin -xzf "${src}" nerdctl
[[ "${src}" = "/tmp/nerdctl-${NERDCTL_VERSION}-linux-arm64.tar.gz" ]] && rm "${src}"

echo "==> Installing kata-go-static ${KATA_VERSION}..."
src=$(cached_or_download "kata-go-static-${KATA_VERSION}-arm64.tar.zst" \
    "https://github.com/kata-containers/kata-containers/releases/download/${KATA_VERSION}/kata-go-static-${KATA_VERSION}-arm64.tar.zst")
sudo tar -C / -xf "${src}"
[[ "${src}" = "/tmp/kata-go-static-${KATA_VERSION}-arm64.tar.zst" ]] && rm "${src}"

apply_pmu_fix

echo "==> Installing CNI plugins ${CNI_PLUGINS_VERSION}..."
src=$(cached_or_download "cni-plugins-${CNI_PLUGINS_VERSION}-linux-arm64.tgz" \
    "https://github.com/containernetworking/plugins/releases/download/v${CNI_PLUGINS_VERSION}/cni-plugins-linux-arm64-v${CNI_PLUGINS_VERSION}.tgz")
sudo mkdir -p /opt/cni/bin
sudo tar -C /opt/cni/bin -xzf "${src}"
[[ "${src}" = "/tmp/cni-plugins-${CNI_PLUGINS_VERSION}-linux-arm64.tgz" ]] && rm "${src}"

echo "==> Setting up kata shim and wrapper scripts..."
sudo ln -sf /opt/kata/bin/containerd-shim-kata-v2 /usr/local/bin/containerd-shim-kata-v2

printf '#!/bin/bash\nexec sudo /usr/local/bin/nerdctl run --rm --runtime io.containerd.kata.v2 alpine uname -r\n' \
    | sudo tee /usr/local/bin/kata-smoke-test > /dev/null
sudo chmod +x /usr/local/bin/kata-smoke-test

printf '#!/bin/bash\nif test -c /dev/kvm; then echo "ok"; else echo "MISSING" >&2; exit 1; fi\n' \
    | sudo tee /usr/local/bin/kata-check-kvm > /dev/null
sudo chmod +x /usr/local/bin/kata-check-kvm

printf 'lima ALL=(ALL) NOPASSWD: ALL\n' | sudo tee /etc/sudoers.d/kata-dev > /dev/null
sudo chmod 0440 /etc/sudoers.d/kata-dev

sudo systemctl restart containerd

echo "==> Installing Rust ${RUST_VERSION}..."
if [[ ! -d "${HOME}/.cargo" ]]; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
        sh -s -- -y --default-toolchain "${RUST_VERSION}" --no-modify-path
fi
# shellcheck disable=SC2016  # $PATH/$HOME must be written literally, not expanded
printf 'export PATH=$PATH:/usr/local/go/bin:$HOME/go/bin:$HOME/.cargo/bin\n' \
    | sudo tee /etc/profile.d/kata-dev.sh > /dev/null

sudo mkdir -p /var/lib/kata-dev
sudo touch /var/lib/kata-dev/provisioned

echo "==> Provision complete."
