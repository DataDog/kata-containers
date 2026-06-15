#!/usr/bin/env bash
# Test hygiene: remove the e2e pod, any host-sidecar runc state, and orphaned
# sidecar processes left by forced deletes during iteration. Run inside the VM.
set -uo pipefail

PROC="sleep 100000"
RUNC_ROOT=/run/kata-hostsidecar

sudo k3s kubectl delete pod hostsidecar-e2e --ignore-not-found --force --grace-period=0 >/dev/null 2>&1 || true
sleep 2
for id in $(sudo runc --root "$RUNC_ROOT" list -q 2>/dev/null); do
  sudo runc --root "$RUNC_ROOT" delete --force "$id" 2>/dev/null || true
done
# kill orphaned sidecar inits by exact argv (pgrep -f on the full command),
# excluding this script's own pid.
for pid in $(pgrep -f "$PROC" || true); do
  [ "$pid" = "$$" ] && continue
  sudo kill -9 "$pid" 2>/dev/null || true
done
sleep 1
echo "remaining sidecar procs: $(pgrep -cf "$PROC" || echo 0)"
