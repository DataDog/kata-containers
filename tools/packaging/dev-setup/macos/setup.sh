#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VM_NAME="kata-dev"
VM_CONFIG="${SCRIPT_DIR}/kata-dev.yaml"
PROVISION="${SCRIPT_DIR}/provision.sh"
CACHE_DIR="${SCRIPT_DIR}/cache"
INSTALL_K3S=true
BUILD_COMPONENTS=""

for arg in "$@"; do
    case "${arg}" in
        --no-k3s)  INSTALL_K3S=false ;;
        --build)   BUILD_COMPONENTS="agent,shim-v2,runtime" ;;
        --build=*) BUILD_COMPONENTS="${arg#--build=}" ;;
        *)         echo "unknown argument: ${arg}" >&2; exit 1 ;;
    esac
done

# shellcheck source=lib-preflight.sh
source "${SCRIPT_DIR}/lib-preflight.sh"
preflight_checks

# Read Go and Rust versions from the checkout's versions.yaml.
VERSIONS_YAML="${SCRIPT_DIR}/../../../../versions.yaml"
GO_VERSION=$(awk '/^languages:/{f=1} f && /^  golang:/{g=1} g && /^ *version: /{gsub(/"/, "", $2); print $2; exit}' "${VERSIONS_YAML}")
RUST_VERSION=$(awk '/^languages:/{f=1} f && /^  rust:/{r=1} r && /^ *version: /{gsub(/"/, "", $2); print $2; exit}' "${VERSIONS_YAML}")
if [[ -z "${GO_VERSION}" || -z "${RUST_VERSION}" ]]; then
    echo "error: failed to extract versions from ${VERSIONS_YAML} — check YAML structure" >&2
    exit 1
fi

# These have no versions.yaml entry — keep in sync manually when updating.
NERDCTL_VERSION="2.3.1"
K3S_VERSION="v1.36.1+k3s1"
CNI_PLUGINS_VERSION="1.6.0"

# Pin the published baseline independently of checkout tags (which may belong
# to a fork or be moving markers such as last-dd-base).
KATA_VERSION="4.2.0"

fetch() {
    local url="$1" dest="$2"
    [[ -f "${dest}" ]] && return
    echo "Caching $(basename "${dest}")..."
    curl -fL --progress-bar -C - -o "${dest}.tmp" "${url}"
    mv "${dest}.tmp" "${dest}"
}

mkdir -p "${CACHE_DIR}"
fetch "https://go.dev/dl/go${GO_VERSION}.linux-arm64.tar.gz" \
    "${CACHE_DIR}/go${GO_VERSION}.linux-arm64.tar.gz"
fetch "https://github.com/containerd/nerdctl/releases/download/v${NERDCTL_VERSION}/nerdctl-${NERDCTL_VERSION}-linux-arm64.tar.gz" \
    "${CACHE_DIR}/nerdctl-${NERDCTL_VERSION}-linux-arm64.tar.gz"
fetch "https://github.com/kata-containers/kata-containers/releases/download/${KATA_VERSION}/kata-go-static-${KATA_VERSION}-arm64.tar.zst" \
    "${CACHE_DIR}/kata-go-static-${KATA_VERSION}-arm64.tar.zst"
fetch "https://github.com/containernetworking/plugins/releases/download/v${CNI_PLUGINS_VERSION}/cni-plugins-linux-arm64-v${CNI_PLUGINS_VERSION}.tgz" \
    "${CACHE_DIR}/cni-plugins-${CNI_PLUGINS_VERSION}-linux-arm64.tgz"
if ${INSTALL_K3S}; then
    fetch "https://github.com/k3s-io/k3s/releases/download/${K3S_VERSION}/k3s-arm64" \
        "${CACHE_DIR}/k3s-${K3S_VERSION}-arm64"
fi

existing=$(limactl list -q 2>/dev/null | grep "^${VM_NAME}$" || true)
if [[ -n "${existing}" ]]; then
    status=$(limactl list --format '{{.Name}} {{.Status}}' 2>/dev/null | awk "/^${VM_NAME} / {print \$2}")
    if [[ "${status}" != "Running" ]]; then
        echo "VM '${VM_NAME}' exists but stopped — starting..."
        limactl start --yes "${VM_NAME}"
    fi
else
    echo "Creating VM '${VM_NAME}'... (edit kata-dev.yaml to change configuration)"
    limactl start --yes --name "${VM_NAME}" "${VM_CONFIG}"
fi

if limactl shell "${VM_NAME}" -- test -f /var/lib/kata-dev/provisioned &>/dev/null; then
    echo "VM already provisioned — skipping provision step"
else
    echo "Provisioning VM..."
    limactl shell "${VM_NAME}" -- env \
        GO_VERSION="${GO_VERSION}" \
        NERDCTL_VERSION="${NERDCTL_VERSION}" \
        KATA_VERSION="${KATA_VERSION}" \
        RUST_VERSION="${RUST_VERSION}" \
        CNI_PLUGINS_VERSION="${CNI_PLUGINS_VERSION}" \
        CACHE_DIR="${CACHE_DIR}" \
        bash < "${PROVISION}"
fi

if [[ -n "${BUILD_COMPONENTS}" ]]; then
    echo "Building from source: ${BUILD_COMPONENTS}"
    limactl shell "${VM_NAME}" -- env \
        REPO_ROOT="$(cd "${SCRIPT_DIR}/../../../.." && pwd)" \
        BUILD_COMPONENTS="${BUILD_COMPONENTS}" \
        bash < "${SCRIPT_DIR}/build.sh"
fi

if ${INSTALL_K3S}; then
    if limactl shell "${VM_NAME}" -- bash -c "command -v k3s &>/dev/null && sudo k3s kubectl get nodes &>/dev/null" 2>/dev/null; then
        echo "k3s already installed — skipping"
    else
        echo "Installing k3s..."
        limactl shell "${VM_NAME}" -- env \
            K3S_VERSION="${K3S_VERSION}" \
            CACHE_DIR="${CACHE_DIR}" \
            bash < "${SCRIPT_DIR}/install-k3s-inner.sh"
    fi
    echo "Exporting kubeconfig..."
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
fi

echo ""
echo "Verifying environment..."
exec "${SCRIPT_DIR}/verify.sh"
