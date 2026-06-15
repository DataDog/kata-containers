#!/usr/bin/env bash
#
# Copyright (c) 2026 Datadog, Inc.
# SPDX-License-Identifier: Apache-2.0
#
# End-to-end check for host-sidecar routing. Run inside the VM after deploy.sh.
# Asserts that an annotated sidecar runs as a host process via the host OCI
# runtime, while the workload runs inside the guest VM.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
POD="$HERE/pod.yaml"
K="sudo k3s kubectl"
RUNC_ROOT=/run/kata-hostsidecar

cleanup() {
  $K delete -f "$POD" --ignore-not-found --wait=false >/dev/null 2>&1 || true
}
trap cleanup EXIT

fail() { echo "FAIL: $*" >&2; exit 1; }

# Remove any leftover pod from a previous run so stale host processes don't
# confuse the process-count assertions.
${K} delete -f "${POD}" --ignore-not-found --wait=true --timeout=30s >/dev/null 2>&1 || true

echo "==> applying pod"
$K apply -f "$POD"

echo "==> waiting for pod Ready (180s)"
if ! $K wait --for=condition=Ready pod/hostsidecar-e2e --timeout=180s; then
  $K describe pod hostsidecar-e2e | tail -40 >&2
  fail "pod did not become Ready"
fi

echo "==> [1/4] host sidecar is tracked by the host OCI runtime"
if ! sudo runc --root "$RUNC_ROOT" list 2>/dev/null | grep -q running; then
  sudo runc --root "$RUNC_ROOT" list 2>&1 >&2 || true
  fail "no running container under $RUNC_ROOT (sidecar was not routed to the host)"
fi
sudo runc --root "$RUNC_ROOT" list
# Capture the sidecar's host PID from runc state for use in [2/4] and [4/4].
SIDECAR_PID=$(sudo runc --root "$RUNC_ROOT" list -f json 2>/dev/null \
  | python3 -c "import sys,json; cs=json.load(sys.stdin); print(cs[0]['pid'] if cs else 0)" 2>/dev/null || echo 0)
[[ "${SIDECAR_PID}" != "0" ]] || fail "could not determine sidecar PID from runc state"

echo "==> [2/4] sidecar process is alive on the host (workload's sleep runs in the VM)"
# Use kill -0 against the exact PID from runc — avoids matching bash processes that
# have 'sleep 100000' as a substring of their command-line arguments.
sudo kill -0 "${SIDECAR_PID}" 2>/dev/null \
  || fail "sidecar PID ${SIDECAR_PID} is not alive on the host"
echo "    sidecar PID=${SIDECAR_PID} is alive"

echo "==> [3/4] workload runs inside the guest VM (different kernel from host)"
host_kernel=$(uname -r)
wl_kernel=$($K exec hostsidecar-e2e -c workload -- uname -r 2>/dev/null | tr -d "\r")
echo "    host kernel=$host_kernel  workload kernel=$wl_kernel"
[ -n "$wl_kernel" ] || fail "could not read workload kernel"
[ "$wl_kernel" != "$host_kernel" ] || fail "workload kernel == host kernel (workload not isolated in VM)"

echo "==> [3b/4] VM memory is reduced by the host-sidecar allocation (M3)"
# pod.yaml sets host-sidecar-mem-bytes=67108864 (64 MiB). The default VM is
# 2048 MiB; after subtraction it should be at most 1984 MiB.
vm_total_kb=$($K exec hostsidecar-e2e -c workload -- grep MemTotal /proc/meminfo | awk '{print $2}')
vm_total_mb=$(( vm_total_kb / 1024 ))
echo "    VM MemTotal=${vm_total_mb} MiB (expect ≤ 1984, i.e. < 2048 default)"
[ "$vm_total_mb" -lt 2048 ] || fail "VM MemTotal ${vm_total_mb} MiB >= 2048 MiB default (host-sidecar mem subtraction had no effect)"

echo "==> [4/4] teardown removes the host sidecar cleanly"
$K delete -f "$POD" --wait=true --timeout=60s
# Wait up to 10 s for the exact sidecar PID to exit.
for _ in $(seq 1 10); do
  sudo kill -0 "${SIDECAR_PID}" 2>/dev/null || break
  sleep 1
done
if sudo kill -0 "${SIDECAR_PID}" 2>/dev/null; then
  fail "sidecar PID ${SIDECAR_PID} still alive after pod delete"
fi
remaining=$(sudo runc --root "$RUNC_ROOT" list 2>/dev/null | grep -c running || true)
[ "$remaining" = "0" ] || fail "host sidecar left in runc state after delete"

trap - EXIT
echo "PASS: host sidecar routed to host, workload isolated in VM, clean teardown"
