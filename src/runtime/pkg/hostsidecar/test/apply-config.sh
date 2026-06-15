#!/usr/bin/env bash
#
# Copyright (c) 2026 Datadog, Inc.
# SPDX-License-Identifier: Apache-2.0
#
# Switch kata configuration for the proxy-based networking model.
# Run inside the VM (limactl shell kata-dev -- bash <this>). Idempotent.
#
# With internetworking_model=none, the Kata shim creates the tap device in the
# pod's netns but does NOT install TC filters to connect it to eth0.  The host
# sidecar proxy owns forwarding: it installs iptables REDIRECT rules that steer
# all TCP and DNS-UDP from the VM subnet through its local listeners, then
# re-originates the connections via eth0.  Killing the proxy severs VM egress.
#
# Revert with: apply-config.sh --revert
set -euo pipefail

KATA_CONF=/opt/kata/share/defaults/kata-containers/configuration.toml

revert=false
for arg in "$@"; do
  [ "$arg" = "--revert" ] && revert=true
done

patch_toml() {
  local key="$1" val="$2"
  if sudo grep -qE "^${key}" "$KATA_CONF"; then
    sudo sed -i "s|^${key}.*|${key} = \"${val}\"|" "$KATA_CONF"
  else
    echo "${key} = \"${val}\"" | sudo tee -a "$KATA_CONF" >/dev/null
  fi
}

if "$revert"; then
  echo "==> reverting to tcfilter model"
  patch_toml "internetworking_model" "tcfilter"
else
  echo "==> switching to none model for proxy-based networking"
  patch_toml "internetworking_model" "none"
fi

echo "==> restarting k3s to pick up config"
sudo systemctl restart k3s

echo "==> waiting for node Ready"
for _ in $(seq 1 45); do
  if sudo k3s kubectl get nodes 2>/dev/null | grep -q " Ready "; then
    echo "    node Ready"
    exit 0
  fi
  sleep 2
done
echo "node did not become Ready in time" >&2
exit 1
