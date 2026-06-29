#!/usr/bin/env bash
# Runs inside the kata-dev Lima VM via limactl shell.
# Builds kata-containers components from the virtiofs-mounted source tree
# and overlays the results onto the baseline /opt/kata install.
# Env: REPO_ROOT (path to kata-containers checkout), BUILD_COMPONENTS (comma-separated).
set -euo pipefail

: "${REPO_ROOT:?REPO_ROOT must be set}"
: "${BUILD_COMPONENTS:?BUILD_COMPONENTS must be set}"

export PATH=${PATH}:/usr/local/go/bin:${HOME}/go/bin:${HOME}/.cargo/bin
# shellcheck disable=SC1091  # cargo env exists inside the VM, not on the macOS host
[[ -f "${HOME}/.cargo/env" ]] && source "${HOME}/.cargo/env"

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

IFS=',' read -ra COMPONENTS <<< "${BUILD_COMPONENTS}"

for comp in "${COMPONENTS[@]}"; do
    case "${comp}" in
        shim-v2)
            echo "==> Building containerd-shim-kata-v2 (Go)..."
            pushd "${REPO_ROOT}/src/runtime" > /dev/null
            make -B PREFIX=/opt/kata containerd-shim-v2
            sudo install -m755 containerd-shim-kata-v2 /opt/kata/bin/containerd-shim-kata-v2
            sudo ln -sf /opt/kata/bin/containerd-shim-kata-v2 /usr/local/bin/containerd-shim-kata-v2
            popd > /dev/null
            ;;
        runtime)
            echo "==> Building kata-runtime (Go)..."
            pushd "${REPO_ROOT}/src/runtime" > /dev/null
            make -B PREFIX=/opt/kata runtime
            sudo install -m755 kata-runtime /opt/kata/bin/kata-runtime
            popd > /dev/null
            ;;
        agent)
            echo "==> Building kata-agent (Rust)..."
            pushd "${REPO_ROOT}/src/agent" > /dev/null
            make LIBC=gnu
            echo "  Agent built for the VM native GNU toolchain; the baseline guest image is unchanged."
            echo "  To run a custom agent in Kata, rebuild the guest rootfs with"
            echo "  tools/packaging/kata-deploy/local-build/."
            popd > /dev/null
            ;;
        *)
            echo "warning: unknown component '${comp}' — supported: shim-v2, runtime, agent" >&2
            ;;
    esac
done

echo "==> Re-applying PMU fix after build overlay..."
apply_pmu_fix

echo "==> Restarting containerd..."
sudo systemctl restart containerd

echo "==> Build complete."
