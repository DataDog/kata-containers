//go:build linux

// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0

package virtcontainers

import "errors"

type qemuLifecycle int32

const (
	// Zero is conservative: a running or restored VM may own devices.
	qemuMayOwnDevices qemuLifecycle = iota
	qemuNotStarted                  // Fresh VM; no process has inherited device descriptors.
	qemuExited                      // Process exit confirmed; helper cleanup may still fail.
	qemuStopped                     // Process and helper cleanup completed.
)

var _ deviceReleaseReporter = (*qemu)(nil)

func (q *qemu) vmState() qemuLifecycle {
	return qemuLifecycle(q.lifecycle.Load())
}

func (q *qemu) setVMState(state qemuLifecycle) {
	q.lifecycle.Store(int32(state))
}

// devicesReleased reports a lifecycle fact, independently of helper errors.
// A stop timeout leaves MayOwnDevices, never permission to restore a device.
func (q *qemu) devicesReleased() bool {
	switch q.vmState() {
	case qemuNotStarted, qemuExited, qemuStopped:
		return true
	default:
		return false
	}
}

// Only a fresh VM may launch. Check both before preparation and under q.mu
// immediately before launch, since StopVM may run during preparation.
func (q *qemu) checkLaunchState() error {
	switch q.vmState() {
	case qemuNotStarted:
		return nil
	case qemuStopped:
		return errors.New("QEMU was stopped before launch")
	default:
		return errors.New("QEMU can only launch a fresh VM")
	}
}
