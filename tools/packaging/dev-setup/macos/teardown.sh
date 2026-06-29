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
    echo "VM '${VM_NAME}' does not exist — nothing to do"
    rm -f "${SCRIPT_DIR}/kubeconfig.yaml"
    exit 0
fi

status=$(limactl list --format '{{.Name}} {{.Status}}' 2>/dev/null | awk "/^${VM_NAME} / {print \$2}")
if [[ "${status}" = "Running" ]]; then
    echo "Stopping VM '${VM_NAME}'..."
    limactl stop --yes "${VM_NAME}"
fi

echo "Deleting VM '${VM_NAME}'..."
limactl delete --yes "${VM_NAME}"
rm -f "${SCRIPT_DIR}/kubeconfig.yaml"
echo "Done."
