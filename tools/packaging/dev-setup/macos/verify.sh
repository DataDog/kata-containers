#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VM_NAME="kata-dev"
PASS=0
FAIL=0

# shellcheck source=lib-preflight.sh
source "${SCRIPT_DIR}/lib-preflight.sh"
preflight_checks

check() {
    local label="$1" cmd="$2"
    if limactl shell "${VM_NAME}" -- bash -c "${cmd}" &>/dev/null; then
        echo "  [ok] ${label}"
        PASS=$(( PASS + 1 ))
    else
        echo "  [FAIL] ${label}"
        FAIL=$(( FAIL + 1 ))
    fi
}

check_output() {
    local label="$1" cmd="$2" result
    if result=$(limactl shell "${VM_NAME}" -- bash -o pipefail -c "${cmd}" 2>/dev/null) && [[ -n "${result}" ]]; then
        echo "  [ok] ${label}: ${result}"
        PASS=$(( PASS + 1 ))
    else
        echo "  [FAIL] ${label}"
        FAIL=$(( FAIL + 1 ))
    fi
}

echo "=== Verifying kata-dev environment ==="
echo ""

echo "Infrastructure:"
check_output "KVM device"         "kata-check-kvm"
check        "containerd running" "systemctl is-active containerd"
check_output "kernel"             "uname -r"

echo ""
echo "Toolchain:"
check_output "Go"      "source /etc/profile.d/go.sh 2>/dev/null; /usr/local/go/bin/go version 2>/dev/null | awk '{print \$3}'"
check_output "Rust"    "source ~/.cargo/env 2>/dev/null; ~/.cargo/bin/rustc --version 2>/dev/null | awk '{print \$2}'"
check_output "nerdctl" "/usr/local/bin/nerdctl --version 2>/dev/null | awk '{print \$3}'"

echo ""
echo "Kata artifacts:"
check        "kata-runtime binary"  "test -x /opt/kata/bin/kata-runtime"
check        "containerd kata shim" "test -x /usr/local/bin/containerd-shim-kata-v2"
check        "guest kernel"         "test -f /opt/kata/share/kata-containers/vmlinux.container"
check        "guest rootfs"         "test -f /opt/kata/share/kata-containers/kata-containers.img"
check        "kata configuration"   "test -f /opt/kata/share/defaults/kata-containers/configuration.toml"

echo ""
echo "Repo mounts:"
check_output "macOS home accessible" \
    "test -d ${HOME} && echo present"

echo ""
echo "Kata smoke test (this may take ~30s):"
KERNEL_VER=""
SMOKE_OK=false
if KERNEL_VER=$(limactl shell "${VM_NAME}" -- kata-smoke-test); then
    SMOKE_OK=true
fi
HOST_KERNEL=$(limactl shell "${VM_NAME}" -- bash -c "uname -r" 2>/dev/null || true)
if ${SMOKE_OK} && [[ -n "${KERNEL_VER}" ]] && [[ -n "${HOST_KERNEL}" ]] && [[ "${KERNEL_VER}" != "${HOST_KERNEL}" ]]; then
    echo "  [ok] Kata container ran with guest kernel: ${KERNEL_VER} (host: ${HOST_KERNEL})"
    PASS=$(( PASS + 1 ))
else
    echo "  [FAIL] Kata smoke test (got: '${KERNEL_VER}', host: '${HOST_KERNEL}')"
    FAIL=$(( FAIL + 1 ))
fi

# An installed but stopped/broken k3s must fail verification too.
if limactl shell "${VM_NAME}" -- bash -c "command -v k3s" &>/dev/null; then
    echo ""
    echo "Kubernetes (k3s):"
    check        "k3s service"       "systemctl is-active k3s"
    check        "node ready"        "kata-k8s wait nodes --all --for=condition=Ready --timeout=120s"
    check        "kata RuntimeClass" "kata-k8s get runtimeclass kata &>/dev/null"

    echo ""
    echo "Kata-in-Kubernetes smoke test (this may take ~90s):"
    K8S_KERNEL=""
    K8S_SMOKE_OK=false
    if K8S_KERNEL=$(limactl shell "${VM_NAME}" -- kata-k8s-smoke); then
        K8S_SMOKE_OK=true
    fi
    if ${K8S_SMOKE_OK} && [[ -n "${K8S_KERNEL}" ]] && [[ -n "${HOST_KERNEL}" ]] && [[ "${K8S_KERNEL}" != "${HOST_KERNEL}" ]]; then
        echo "  [ok] Kata pod ran with guest kernel: ${K8S_KERNEL}"
        PASS=$(( PASS + 1 ))
    else
        echo "  [FAIL] Kata k8s smoke test (got: '${K8S_KERNEL}')"
        FAIL=$(( FAIL + 1 ))
    fi
fi

echo ""
echo "Result: ${PASS} passed, ${FAIL} failed"
[[ "${FAIL}" -eq 0 ]]
