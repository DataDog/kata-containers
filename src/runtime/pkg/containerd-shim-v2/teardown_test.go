// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0

package containerdshim

import (
	"context"
	"errors"
	"sync"
	"testing"
	"time"

	taskAPI "github.com/containerd/containerd/api/runtime/task/v2"
	vc "github.com/kata-containers/kata-containers/src/runtime/virtcontainers"
	"github.com/kata-containers/kata-containers/src/runtime/virtcontainers/pkg/vcmock"
	"github.com/stretchr/testify/require"
)

func TestTeardownConcurrentRequests(t *testing.T) {
	for _, fail := range []bool{false, true} {
		name := "success"
		var phaseErr error
		if fail {
			name = "failed phases are not repeated"
			phaseErr = errors.New("teardown failure")
		}
		t.Run(name, func(t *testing.T) {
			var calls []string
			s := &service{sandbox: &vcmock.Sandbox{
				MockID:     testSandboxID,
				StopFunc:   func(bool) error { calls = append(calls, "stop"); return phaseErr },
				DeleteFunc: func() error { calls = append(calls, "delete"); return phaseErr },
			}}
			var requests sync.WaitGroup
			for i := 0; i < 20; i++ {
				requests.Add(1)
				go func(target sandboxTeardownPhase) {
					defer requests.Done()
					s.advanceTeardown(context.Background(), target)
				}(teardownStopDone + sandboxTeardownPhase(i%2))
			}
			requests.Wait()
			require.Equal(t, []string{"stop", "delete"}, calls)
			require.Equal(t, teardownDeleteDone, s.teardownPhase)
		})
	}
}

// The watcher holds s.mu during teardown. This covers its serialization with
// the waiter and eventual exit publication; phase locking is tested separately.
func TestWaitTeardownWatcherWins(t *testing.T) {
	s := &service{
		id:         testSandboxID,
		containers: make(map[string]*container),
		ec:         make(chan exit, 1),
		monitor:    make(chan error, 1),
	}
	c, err := newContainer(s, &taskAPI.CreateTaskRequest{ID: testSandboxID}, vc.PodSandbox, nil, false)
	require.NoError(t, err)
	s.containers[testSandboxID] = c
	close(c.exitIOch)
	stopEntered := make(chan struct{})
	allowStop := make(chan struct{})
	var release sync.Once
	finishStop := func() { release.Do(func() { close(allowStop) }) }
	defer finishStop()
	waitStarted := make(chan struct{})
	var calls []string
	s.sandbox = &vcmock.Sandbox{
		MockID:                  testSandboxID,
		WaitProcessFunc:         func(string, string) (int32, error) { return 0, nil },
		HasPhysicalEndpointFunc: func() bool { return true },
		StopFunc: func(bool) error {
			calls = append(calls, "stop")
			close(stopEntered)
			<-allowStop
			return nil
		},
		DeleteFunc: func() error { calls = append(calls, "delete"); return nil },
	}
	watchDone := make(chan struct{})
	s.monitor <- errors.New("QEMU exited unexpectedly")
	go func() { watchSandbox(context.Background(), s); close(watchDone) }()
	select {
	case <-stopEntered:
	case <-time.After(time.Second):
		t.Fatal("watcher did not begin teardown")
	}
	waitDone := make(chan error, 1)
	go func() { close(waitStarted); _, err := wait(context.Background(), s, c, ""); waitDone <- err }()
	select {
	case <-waitStarted:
	case <-time.After(time.Second):
		t.Fatal("container waiter did not start")
	}
	finishStop()
	select {
	case err := <-waitDone:
		require.NoError(t, err)
	case <-time.After(time.Second):
		t.Fatal("container waiter did not complete after watcher teardown")
	}
	<-watchDone
	require.Equal(t, []string{"stop", "delete"}, calls)
	require.Equal(t, teardownDeleteDone, s.teardownPhase)
	require.Len(t, c.exitCh, 1, "exit must be published exactly once")
	require.Equal(t, uint32(0), <-c.exitCh)
}

func TestTeardownStopBeforeDelete(t *testing.T) {
	stopEntered := make(chan struct{})
	allowStop := make(chan struct{})
	deleteEntered := make(chan struct{}, 1)
	var release sync.Once
	finishStop := func() { release.Do(func() { close(allowStop) }) }
	defer finishStop()
	var calls []string
	s := &service{sandbox: &vcmock.Sandbox{
		StopFunc: func(bool) error {
			close(stopEntered)
			<-allowStop
			calls = append(calls, "stop")
			return nil
		},
		DeleteFunc: func() error {
			deleteEntered <- struct{}{}
			calls = append(calls, "delete")
			return nil
		},
	}}
	await := func(done <-chan struct{}) {
		t.Helper()
		select {
		case <-done:
		case <-time.After(time.Second):
			t.Fatal("teardown did not progress")
		}
	}
	stopDone := make(chan struct{})
	go func() { s.advanceTeardown(context.Background(), teardownStopDone); close(stopDone) }()
	await(stopEntered)
	deleteRequested, deleteDone := make(chan struct{}), make(chan struct{})
	go func() {
		close(deleteRequested)
		s.advanceTeardown(context.Background(), teardownDeleteDone)
		close(deleteDone)
	}()
	await(deleteRequested)
	select {
	case <-deleteEntered:
		t.Fatal("Delete overtook an unfinished Stop")
	case <-time.After(20 * time.Millisecond):
	}
	finishStop()
	await(stopDone)
	await(deleteDone)
	// The original waiter resumes after publication; Delete must not repeat.
	s.advanceTeardown(context.Background(), teardownDeleteDone)
	require.Equal(t, []string{"stop", "delete"}, calls)
}
