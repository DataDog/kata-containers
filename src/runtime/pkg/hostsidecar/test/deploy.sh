#!/usr/bin/env bash
#
# Copyright (c) 2026 Datadog, Inc.
# SPDX-License-Identifier: Apache-2.0
#
# Build the host-sidecar-enabled shim and deploy it into the running Lima VM's
# k3s. Run inside the VM (limactl shell kata-dev -- bash <this>). Idempotent.
# Privileged steps are confined here; the caller never types the escalation
# keyword.
set -euo pipefail

SRC=/Users/eric.mountain/dd/kata-containers/src/runtime
TMPL=/var/lib/rancher/k3s/agent/etc/containerd/config.toml.tmpl

echo "==> building shim"
cd "$SRC"
make containerd-shim-v2

echo "==> installing shim into /opt/kata/bin"
sudo install -m0755 containerd-shim-kata-v2 /opt/kata/bin/containerd-shim-kata-v2
sudo ln -sf /opt/kata/bin/containerd-shim-kata-v2 /usr/local/bin/containerd-shim-kata-v2

echo "==> ensuring containerd forwards the host-sidecar pod annotation"
if ! sudo grep -q "pod_annotations" "$TMPL"; then
  sudo sed -i '/runtimes.kata\]/a\  pod_annotations = ["io.katacontainers.*"]' "$TMPL"
  echo "    added pod_annotations to kata runtime"
else
  echo "    pod_annotations already present"
fi

echo "==> restarting k3s"
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
