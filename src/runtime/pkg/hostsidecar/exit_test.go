// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

package hostsidecar

import (
	"context"
	"sync/atomic"
	"testing"
	"time"

	"github.com/containerd/containerd/api/types/task"
	runc "github.com/containerd/go-runc"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// TestContainerExitNotifiesOnce drives the status watcher: the runtime reports
// "running" a few times then "stopped"; the container must record the exit,
// push the code on ExitCh, and invoke OnExit exactly once.
func TestContainerExitNotifiesOnce(t *testing.T) {
	var polls atomic.Int32
	rt := &fakeRuntime{
		stateFn: func() (*runc.Container, error) {
			if polls.Add(1) < 3 {
				return &runc.Container{Pid: 10, Status: "running"}, nil
			}
			return &runc.Container{Pid: 10, Status: "stopped"}, nil
		},
	}

	var onExitCalls atomic.Int32
	c := &Container{
		id:           "c1",
		rt:           rt,
		pollInterval: time.Millisecond,
		exitCh:       make(chan uint32, 1),
		onExit:       func(uint32, time.Time) { onExitCalls.Add(1) },
	}

	require.NoError(t, c.Start(context.Background()))

	select {
	case code := <-c.ExitCh():
		assert.Equal(t, uint32(0), code, "clean exit, no signal observed")
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for exit notification")
	}

	assert.Equal(t, task.Status_STOPPED, c.Status())
	assert.False(t, c.ExitedAt().IsZero())
	// give any spurious extra notifications a chance, then assert single fire.
	time.Sleep(20 * time.Millisecond)
	assert.Equal(t, int32(1), onExitCalls.Load(), "OnExit must fire exactly once")
}

// TestContainerExitCodeFromSignal verifies the best-effort exit code inferred
// from the last delivered signal when the runtime cannot report the real code.
func TestContainerExitCodeFromSignal(t *testing.T) {
	rt := &fakeRuntime{
		stateFn: func() (*runc.Container, error) {
			return &runc.Container{Status: "stopped"}, nil
		},
	}
	c := &Container{
		id:           "c1",
		rt:           rt,
		status:       task.Status_RUNNING,
		pollInterval: time.Millisecond,
		exitCh:       make(chan uint32, 1),
	}
	require.NoError(t, c.Kill(context.Background(), uint32(9), false)) // SIGKILL
	go c.watch()

	select {
	case code := <-c.ExitCh():
		assert.Equal(t, uint32(128+9), code, "SIGKILL -> 137")
	case <-time.After(2 * time.Second):
		t.Fatal("timed out waiting for exit notification")
	}
}

// TestContainerExitOnRuntimeGone treats a vanished container (State error) as
// an exit so waiters are never left hanging if runc state disappears.
func TestContainerExitOnRuntimeGone(t *testing.T) {
	rt := &fakeRuntime{stateErr: assertAnErr}
	c := &Container{
		id:           "c1",
		rt:           rt,
		pollInterval: time.Millisecond,
		exitCh:       make(chan uint32, 1),
	}
	go c.watch()
	select {
	case <-c.ExitCh():
	case <-time.After(2 * time.Second):
		t.Fatal("timed out: vanished container must be reported as exited")
	}
}

var assertAnErr = errPermanent("gone")

type errPermanent string

func (e errPermanent) Error() string { return string(e) }
