#!/usr/bin/env bash
# Revert the debug toggles added by debug-capture.sh (which slow VM boot and can
# trip the agent vsock timeout on the nested ARM VM). Run inside the VM.
set -uo pipefail

TMPL=/var/lib/rancher/k3s/agent/etc/containerd/config.toml.tmpl
KCFG=/opt/kata/share/defaults/kata-containers/configuration.toml

sudo sed -i 's/^enable_debug = true/enable_debug = false/' "$KCFG" || true
# drop the [debug] block (header + its level line) appended to the template
sudo sed -i '/^\[debug\]/,+1d' "$TMPL" || true

sudo systemctl restart k3s
for _ in $(seq 1 45); do sudo k3s kubectl get nodes 2>/dev/null | grep -q " Ready " && break; sleep 2; done
echo "debug reverted; node Ready"
