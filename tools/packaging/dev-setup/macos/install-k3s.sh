#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VM_NAME="kata-dev"
CACHE_DIR="${SCRIPT_DIR}/cache"

# shellcheck source=lib-preflight.sh
source "${SCRIPT_DIR}/lib-preflight.sh"
preflight_checks

if ! limactl shell "${VM_NAME}" -- kata-check-kvm &>/dev/null; then
    echo "error: VM not provisioned — run ./setup.sh first" >&2
    exit 1
fi

if limactl shell "${VM_NAME}" -- bash -c "command -v k3s &>/dev/null && sudo k3s kubectl get nodes &>/dev/null" 2>/dev/null; then
    echo "k3s already installed and running"
    exec "${SCRIPT_DIR}/verify.sh"
fi

echo "Fetching latest k3s version..."
K3S_VERSION=$(curl -fsSL "https://api.github.com/repos/k3s-io/k3s/releases/latest" \
    | python3 -c "import sys,json; print(json.load(sys.stdin)['tag_name'])")
echo "k3s ${K3S_VERSION}"

mkdir -p "${CACHE_DIR}"
K3S_CACHE="${CACHE_DIR}/k3s-${K3S_VERSION}-arm64"
if [[ ! -f "${K3S_CACHE}" ]]; then
    echo "Caching k3s binary..."
    curl -fL --progress-bar \
        "https://github.com/k3s-io/k3s/releases/download/${K3S_VERSION}/k3s-arm64" \
        -o "${K3S_CACHE}.tmp"
    mv "${K3S_CACHE}.tmp" "${K3S_CACHE}"
fi

echo "Installing k3s..."
limactl shell "${VM_NAME}" -- env \
    K3S_VERSION="${K3S_VERSION}" \
    CACHE_DIR="${CACHE_DIR}" \
    bash < "${SCRIPT_DIR}/install-k3s-inner.sh"

echo "Exporting kubeconfig to kubeconfig.yaml..."
limactl shell "${VM_NAME}" -- bash -c "sudo cat /etc/rancher/k3s/k3s.yaml" \
    > "${SCRIPT_DIR}/kubeconfig.yaml"

echo ""
echo "kubectl from macOS (Lima auto-forwards port 6443):"
echo "  export KUBECONFIG=${SCRIPT_DIR}/kubeconfig.yaml"
echo "  kubectl get nodes"
echo ""
echo "Tip: create an .envrc to set KUBECONFIG automatically:"
echo "  echo 'export KUBECONFIG=${SCRIPT_DIR}/kubeconfig.yaml' > ${SCRIPT_DIR}/.envrc"
echo "  direnv allow   # brew install direnv if needed"
