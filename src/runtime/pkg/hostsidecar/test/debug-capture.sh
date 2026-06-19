#!/usr/bin/env bash
# Enable containerd+kata debug, recreate the e2e pod, and capture the shim log
# around the host-proxy Start. Run inside the VM. Diagnostic only.
set -uo pipefail

TMPL=/var/lib/rancher/k3s/agent/etc/containerd/config.toml.tmpl
KCFG=/opt/kata/share/defaults/kata-containers/configuration.toml
HERE="$(cd "$(dirname "$0")" && pwd)"

# containerd debug -> shim opts.Debug=true
if ! sudo grep -q '^\[debug\]' "$TMPL"; then
  printf '\n[debug]\n  level = "debug"\n' | sudo tee -a "$TMPL" >/dev/null
fi
# kata runtime debug
sudo sed -i 's/^#*enable_debug = .*/enable_debug = true/' "$KCFG" || true

sudo systemctl restart k3s
for _ in $(seq 1 45); do sudo k3s kubectl get nodes 2>/dev/null | grep -q " Ready " && break; sleep 2; done

sudo k3s kubectl delete pod hostsidecar-e2e --ignore-not-found --force --grace-period=0 >/dev/null 2>&1 || true
sleep 3
sudo journalctl --vacuum-time=1s >/dev/null 2>&1 || true
sudo k3s kubectl apply -f "$HERE/pod.yaml"

echo "==> waiting 40s for create/start attempts"
sleep 40

echo "===== kata shim log (-t kata) ====="
sudo journalctl -t kata --no-pager --since "-1min" 2>/dev/null | grep -iE "host|Start\(\)|Create\(\)|runc|error|panic|sidecar" | tail -60
echo "===== fallback: k3s journal shim lines ====="
sudo journalctl -u k3s --no-pager --since "-1min" 2>/dev/null | grep -iE "host-sidecar|hostsidecar|runc create|runc start|level=error" | tail -30
