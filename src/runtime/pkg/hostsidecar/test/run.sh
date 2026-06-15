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

echo "==> [2/4] exactly one host-side sleep process (sidecar on host; workload's sleep is in the VM)"
n=$(pgrep -fc "sleep 100000" || true)
[ "$n" = "1" ] || fail "expected exactly 1 host-side 'sleep 100000', found $n"

echo "==> [3/4] workload runs inside the guest VM (different kernel from host)"
host_kernel=$(uname -r)
wl_kernel=$($K exec hostsidecar-e2e -c workload -- uname -r 2>/dev/null | tr -d "\r")
echo "    host kernel=$host_kernel  workload kernel=$wl_kernel"
[ -n "$wl_kernel" ] || fail "could not read workload kernel"
[ "$wl_kernel" != "$host_kernel" ] || fail "workload kernel == host kernel (workload not isolated in VM)"

echo "==> [4/4] teardown removes the host sidecar cleanly"
$K delete -f "$POD" --wait=true --timeout=60s
sleep 3
left=$(pgrep -fc "sleep 100000" || true)
[ "$left" = "0" ] || fail "host sidecar process leaked after delete (found $left)"
remaining=$(sudo runc --root "$RUNC_ROOT" list 2>/dev/null | grep -c running || true)
[ "$remaining" = "0" ] || fail "host sidecar left in runc state after delete"

trap - EXIT
echo "PASS: host sidecar routed to host, workload isolated in VM, clean teardown"
