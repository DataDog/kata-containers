#!/usr/bin/env bash
#
# Copyright (c) 2026 Datadog, Inc.
# SPDX-License-Identifier: Apache-2.0
#
# Build the kata-dev-network-proxy image on the Mac and load it into the
# k3s-embedded containerd inside the Lima VM.
#
# Run on the Mac (not inside the VM):
#   bash load-proxy.sh [--apply]
#
# --apply  also applies proxy-demo.yaml after loading the image.
#
# Background
# ----------
# k3s embeds its own containerd at /run/k3s/containerd/containerd.sock,
# separate from the system containerd at /run/containerd/containerd.sock.
# ctr must be pointed at the k3s socket; otherwise images land in a
# namespace k3s never sees and pods fail with ErrImageNeverPull.
#
# imagePullPolicy: Never is used in proxy-demo.yaml because the image is
# built locally — there is no registry to pull from.  Never tells k3s to
# use whatever is already cached without attempting a registry lookup.
set -euo pipefail

PROXY_SRC="$HOME/go/src/github.com/ddoghq-sandbox/kata-dev-network-proxy"
DEMO_YAML="$(cd "$(dirname "$0")" && pwd)/proxy-demo.yaml"
VM=kata-dev
IMAGE=kata-dev-network-proxy:latest
K3S_CTR_ADDR=/run/k3s/containerd/containerd.sock

APPLY=0
for arg in "$@"; do
  [[ "$arg" == "--apply" ]] && APPLY=1
done

echo "==> building proxy image"
docker build -t "$IMAGE" "$PROXY_SRC"

echo "==> loading into k3s containerd (${VM})"
docker save "$IMAGE" | limactl shell "$VM" -- bash -c "
  set -euo pipefail
  sudo ctr --address $K3S_CTR_ADDR -n k8s.io images import -

  # Tag with the short name so imagePullPolicy: Never resolves without
  # the docker.io/library/ prefix that ctr import adds.
  sudo ctr --address $K3S_CTR_ADDR -n k8s.io images tag \
    docker.io/library/$IMAGE \
    $IMAGE 2>/dev/null || true
  echo '    images in k3s containerd:'
  sudo ctr --address $K3S_CTR_ADDR -n k8s.io images list | grep kata-dev-network-proxy | awk '{print \"      \" \$1}'
"

echo "==> proxy image ready in ${VM}"

if [[ "$APPLY" -eq 1 ]]; then
  echo "==> applying proxy-demo.yaml"
  limactl shell "$VM" -- bash -c "
    sudo k3s kubectl apply -f $DEMO_YAML
    sudo k3s kubectl wait --for=condition=Ready pod/proxy-demo --timeout=180s
  "
  echo "==> proxy-demo pod ready"
fi
