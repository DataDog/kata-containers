#!/usr/bin/env bash
#
# Copyright (c) 2026 Datadog, Inc.
# SPDX-License-Identifier: Apache-2.0
#
# Switch the Kata internetworking_model for host-sidecar proxy networking.
# Run inside the VM (limactl shell kata-dev -- bash <this>). Idempotent.
#
# Modes
# -----
#   (no args)  tapnet   — tap in pod netns; proxy drives gvisor-tap-vsock.
#                         No iptables rules; kill-9 proof. [DEFAULT]
#   --iptables           — tap in jail netns + veth; proxy uses iptables REDIRECT.
#   --revert             — restore tcfilter (standard Kata, no host sidecar).
#
set -euo pipefail

KATA_CONF=/opt/kata/share/defaults/kata-containers/configuration.toml

model="tapnet"
for arg in "$@"; do
  case "$arg" in
    --iptables) model="none" ;;
    --revert)   model="tcfilter" ;;
  esac
done

patch_toml() {
  local key="$1" val="$2"
  if sudo grep -qE "^${key}" "$KATA_CONF"; then
    sudo sed -i "s|^${key}.*|${key} = \"${val}\"|" "$KATA_CONF"
  else
    echo "${key} = \"${val}\"" | sudo tee -a "$KATA_CONF" >/dev/null
  fi
}

echo "==> setting internetworking_model = ${model}"
patch_toml "internetworking_model" "$model"

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
