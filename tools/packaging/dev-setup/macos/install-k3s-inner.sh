#!/usr/bin/env bash
# Runs inside the kata-dev Lima VM via limactl shell.
set -euo pipefail

: "${K3S_VERSION:?K3S_VERSION must be set}"
: "${CACHE_DIR:?CACHE_DIR must be set}"

echo "==> Installing k3s binary from cache..."
sudo install -m755 "${CACHE_DIR}/k3s-${K3S_VERSION}-arm64" /usr/local/bin/k3s

echo "==> Creating containerd config template with Kata runtime..."
sudo mkdir -p /var/lib/rancher/k3s/agent/etc/containerd/
sudo tee /var/lib/rancher/k3s/agent/etc/containerd/config-v3.toml.tmpl > /dev/null << 'TMPL'
{{ template "base" . }}

[plugins."io.containerd.cri.v1.runtime".containerd.runtimes.kata]
  runtime_type = "io.containerd.kata.v2"

[plugins."io.containerd.cri.v1.runtime".containerd.runtimes.kata.options]
  ConfigPath = "/opt/kata/share/defaults/kata-containers/configuration.toml"
TMPL

echo "==> Running k3s service installer..."
curl -sfL https://get.k3s.io | \
    INSTALL_K3S_VERSION="${K3S_VERSION}" \
    INSTALL_K3S_SKIP_DOWNLOAD=true \
    INSTALL_K3S_EXEC="server
      --disable=traefik
      --disable=metrics-server
      --write-kubeconfig-mode=644" \
    sh -

echo "==> Waiting for k3s node to be Ready..."
if ! timeout 120 bash -c \
    'until sudo k3s kubectl get nodes 2>&1 | grep -q " Ready"; do sleep 3; done'; then
    echo "error: k3s node did not become Ready within 120s" >&2
    echo "  check: sudo k3s kubectl get nodes" >&2
    exit 1
fi

echo "==> Applying RuntimeClass (kata)..."
sudo k3s kubectl apply -f - << 'EOF'
apiVersion: node.k8s.io/v1
kind: RuntimeClass
metadata:
  name: kata
handler: kata
EOF

echo "==> Installing kubectl wrapper and smoke test scripts..."
printf '#!/bin/bash\nexec sudo k3s kubectl "$@"\n' \
    | sudo tee /usr/local/bin/kata-k8s > /dev/null
sudo chmod +x /usr/local/bin/kata-k8s

sudo tee /usr/local/bin/kata-k8s-smoke > /dev/null << 'SCRIPT'
#!/bin/bash
set -euo pipefail
sudo k3s kubectl delete pod kata-k8s-smoke --ignore-not-found --timeout=60s >/dev/null
trap 'sudo k3s kubectl delete pod kata-k8s-smoke --ignore-not-found --wait=false >/dev/null' EXIT
sudo k3s kubectl run kata-k8s-smoke --image=alpine --restart=Never \
    --overrides='{"spec":{"runtimeClassName":"kata"}}' -- uname -r >/dev/null
if ! sudo k3s kubectl wait pod/kata-k8s-smoke \
    --for=jsonpath='{.status.phase}'=Succeeded --timeout=120s >/dev/null; then
    sudo k3s kubectl describe pod kata-k8s-smoke >&2
    exit 1
fi
sudo k3s kubectl logs kata-k8s-smoke
SCRIPT
sudo chmod +x /usr/local/bin/kata-k8s-smoke

echo "==> k3s ready."
sudo k3s kubectl get nodes
