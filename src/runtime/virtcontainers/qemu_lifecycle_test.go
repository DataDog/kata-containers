//go:build linux

// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0

package virtcontainers

import (
	"context"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"syscall"
	"testing"
	"testing/synctest"

	"github.com/kata-containers/kata-containers/src/runtime/pkg/device/config"
	govmmQemu "github.com/kata-containers/kata-containers/src/runtime/pkg/govmm/qemu"
	"github.com/stretchr/testify/require"
)

type retryVirtiofsDaemon struct {
	virtiofsdMock
	stopErr error
	stops   int
}

func (v *retryVirtiofsDaemon) Stop(context.Context) error {
	v.stops++
	return v.stopErr
}

func TestQemuLifecycleHelperFailurePreservesExit(t *testing.T) {
	// The recorded PID now belongs to a different live process. Once the
	// QEMU reaper confirmed exit, neither a stop nor its retry may signal it.
	reusedPID := exec.Command("sleep", "300")
	require.NoError(t, reusedPID.Start())
	t.Cleanup(func() { reusedPID.Process.Kill(); reusedPID.Wait() })
	dir := t.TempDir()
	pidFile := filepath.Join(dir, "pid")
	require.NoError(t, os.WriteFile(pidFile, []byte(strconv.Itoa(reusedPID.Process.Pid)), 0600))
	helperErr := errors.New("helper failed to stop")
	helper := &retryVirtiofsDaemon{stopErr: helperErr}
	q := &qemu{
		id: "testSandbox", ctx: context.Background(),
		config:         HypervisorConfig{VMStorePath: dir, RunStorePath: dir, SharedFS: config.VirtioFS},
		qemuConfig:     govmmQemu.Config{PidFile: pidFile},
		virtiofsDaemon: helper,
		exitCh:         make(chan struct{}),
	}
	q.state.VirtiofsDaemonPid = 1
	q.setVMState(qemuMayOwnDevices)
	close(q.exitCh)

	require.ErrorIs(t, q.StopVM(context.Background(), false), helperErr)
	require.Equal(t, qemuExited, q.vmState())
	require.True(t, q.devicesReleased())
	require.NoError(t, reusedPID.Process.Signal(syscall.Signal(0)))

	helper.stopErr = nil
	require.NoError(t, q.StopVM(context.Background(), false))
	require.Equal(t, qemuStopped, q.vmState())
	require.True(t, q.devicesReleased())
	require.NoError(t, reusedPID.Process.Signal(syscall.Signal(0)))
	require.NoError(t, q.StopVM(context.Background(), false))
	require.Equal(t, 2, helper.stops, "completed helper cleanup must not repeat")
	require.Error(t, q.Check())
	require.Empty(t, q.Save())
}

func TestQemuLifecycleUnconfirmedExitCanBeRetried(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		dir := t.TempDir()
		q := &qemu{
			id: "testSandbox", ctx: context.Background(),
			config:     HypervisorConfig{VMStorePath: dir, RunStorePath: dir},
			qemuConfig: govmmQemu.Config{PidFile: filepath.Join(dir, "missing-pid")},
			exitCh:     make(chan struct{}),
		}
		q.setVMState(qemuMayOwnDevices)
		require.ErrorIs(t, q.StopVM(context.Background(), false), errVMMExitUnconfirmed)
		require.Equal(t, qemuMayOwnDevices, q.vmState())
		require.False(t, q.devicesReleased())

		close(q.exitCh)
		require.NoError(t, q.StopVM(context.Background(), false))
		require.Equal(t, qemuStopped, q.vmState())
		require.True(t, q.devicesReleased(), "later exit confirmation must allow restoration")
	})
}

func TestQemuLifecycleRestoredProcessWithoutQMP(t *testing.T) {
	process := exec.Command("sleep", "300")
	require.NoError(t, process.Start())
	t.Cleanup(func() { process.Process.Kill(); process.Wait() })
	dir := t.TempDir()
	pidFile := filepath.Join(dir, "pid")
	require.NoError(t, os.WriteFile(pidFile, []byte(strconv.Itoa(process.Process.Pid)), 0600))
	q := &qemu{
		id: "testSandbox", ctx: context.Background(),
		config:     HypervisorConfig{VMStorePath: dir, RunStorePath: dir},
		qemuConfig: govmmQemu.Config{PidFile: pidFile},
	}
	q.qmpMonitorCh.ctx = context.Background()
	q.qmpMonitorCh.path = filepath.Join(dir, "missing-qmp.sock")
	require.ErrorIs(t, q.StopVM(context.Background(), false), errVMMExitUnconfirmed)
	require.Equal(t, qemuMayOwnDevices, q.vmState())
	require.False(t, q.devicesReleased())
	require.NoError(t, process.Process.Signal(syscall.Signal(0)))
}

func TestQemuLifecycleRejectsRelaunch(t *testing.T) {
	for _, state := range []qemuLifecycle{qemuMayOwnDevices, qemuExited, qemuStopped} {
		t.Run(strconv.Itoa(int(state)), func(t *testing.T) {
			dir := t.TempDir()
			q := &qemu{id: "must-not-launch", config: HypervisorConfig{VMStorePath: dir}}
			q.setVMState(state)
			require.Error(t, q.StartVM(context.Background(), 1))
			require.Equal(t, state, q.vmState(), "rejection must preserve device-ownership state")
			require.NoDirExists(t, filepath.Join(dir, q.id), "rejection must precede launch preparation")
		})
	}
}

// A post-launch failure can remove the PID and QMP files before the process
// reaper completes. Startup rollback must still await its exit confirmation.
func TestQemuLifecycleMissingPIDWaitsForReaper(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		dir := t.TempDir()
		q := &qemu{
			id: "testSandbox", ctx: context.Background(),
			config:     HypervisorConfig{VMStorePath: dir, RunStorePath: dir},
			qemuConfig: govmmQemu.Config{PidFile: filepath.Join(dir, "missing-pid")},
			exitCh:     make(chan struct{}),
		}
		q.qmpMonitorCh.ctx = context.Background()
		q.qmpMonitorCh.path = filepath.Join(dir, "missing-qmp.sock")
		stopped := make(chan error, 1)
		go func() { stopped <- q.StopVM(context.Background(), false) }()
		synctest.Wait()
		select {
		case err := <-stopped:
			t.Fatalf("StopVM returned before the reaper confirmed exit: %v", err)
		default:
		}
		require.False(t, q.devicesReleased())
		close(q.exitCh)
		require.NoError(t, <-stopped)
		require.True(t, q.reaped(), "successful stop must observe the reaper")
		require.Equal(t, qemuStopped, q.vmState())
		require.True(t, q.devicesReleased(), "startup rollback may now restore its devices")
	})
}
