#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VM_NAME="kata-dev"

# shellcheck source=lib-preflight.sh
source "${SCRIPT_DIR}/lib-preflight.sh"
_pf_macos
_pf_apple_silicon
_pf_lima

existing=$(limactl list -q 2>/dev/null | grep "^${VM_NAME}$" || true)
if [[ -z "${existing}" ]]; then
    echo "VM '${VM_NAME}' does not exist — nothing to do"; exit 0
fi

status=$(limactl list --format '{{.Name}} {{.Status}}' 2>/dev/null | awk "/^${VM_NAME} / {print \$2}")
if [[ "${status}" != "Running" ]]; then
    echo "VM not running — nothing to do"; exit 0
fi

if ! limactl shell "${VM_NAME}" -- bash -c "command -v k3s &>/dev/null"; then
    echo "k3s not installed — nothing to do"; exit 0
fi

echo "Uninstalling k3s..."
if limactl shell "${VM_NAME}" -- bash -c "test -f /usr/local/bin/k3s-uninstall.sh"; then
    limactl shell "${VM_NAME}" -- sudo /usr/local/bin/k3s-uninstall.sh
fi

rm -f "${SCRIPT_DIR}/kubeconfig.yaml"
echo "Done."
