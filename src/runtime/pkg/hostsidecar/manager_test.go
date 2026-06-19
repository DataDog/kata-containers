// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

package hostsidecar

import (
	"context"
	"errors"
	"testing"

	"github.com/containerd/containerd/api/types/task"
	runc "github.com/containerd/go-runc"
	specs "github.com/opencontainers/runtime-spec/specs-go"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// fakeRuntime records calls and returns scripted results, satisfying OCIRuntime
// without a real runc binary so the manager FSM is unit-testable.
type fakeRuntime struct {
	createErr error
	startErr  error
	killErr   error
	deleteErr error
	state     *runc.Container
	stateErr  error
	// stateFn, if set, overrides state/stateErr and is called per State call.
	stateFn func() (*runc.Container, error)

	created  []string
	started  []string
	killed   []killCall
	deleted  []string
	createIO bool
}

type killCall struct {
	id  string
	sig int
	all bool
}

func (f *fakeRuntime) Create(_ context.Context, id, _ string, opts *runc.CreateOpts) error {
	f.created = append(f.created, id)
	if opts != nil && opts.IO != nil {
		f.createIO = true
	}
	return f.createErr
}
func (f *fakeRuntime) Start(_ context.Context, id string) error {
	f.started = append(f.started, id)
	return f.startErr
}
func (f *fakeRuntime) State(_ context.Context, id string) (*runc.Container, error) {
	if f.stateFn != nil {
		return f.stateFn()
	}
	if f.stateErr != nil {
		return nil, f.stateErr
	}
	return f.state, nil
}
func (f *fakeRuntime) Kill(_ context.Context, id string, sig int, opts *runc.KillOpts) error {
	all := opts != nil && opts.All
	f.killed = append(f.killed, killCall{id, sig, all})
	return f.killErr
}
func (f *fakeRuntime) Delete(_ context.Context, id string, _ *runc.DeleteOpts) error {
	f.deleted = append(f.deleted, id)
	return f.deleteErr
}
func (f *fakeRuntime) Exec(_ context.Context, _ string, _ specs.Process, _ *runc.ExecOpts) error {
	return nil
}
func (f *fakeRuntime) Update(_ context.Context, _ string, _ *specs.LinuxResources) error {
	return nil
}
func (f *fakeRuntime) Stats(_ context.Context, _ string) (*runc.Stats, error) {
	return &runc.Stats{}, nil
}
func (f *fakeRuntime) Pause(_ context.Context, _ string) error  { return nil }
func (f *fakeRuntime) Resume(_ context.Context, _ string) error { return nil }

func newTestManager(rt OCIRuntime) *Manager {
	return newManagerWithRuntime(Config{Enabled: true}, rt)
}

func validParams(t *testing.T) CreateParams {
	t.Helper()
	return CreateParams{
		ID:        "c1",
		SandboxID: "sb",
		Bundle:    t.TempDir(),
		Spec:      &specs.Spec{Linux: &specs.Linux{}},
		NetnsPath: "/run/netns/pod",
	}
}

func TestManagerCreate(t *testing.T) {
	rt := &fakeRuntime{}
	m := newTestManager(rt)

	p := validParams(t)
	c, err := m.Create(context.Background(), p)
	require.NoError(t, err)
	assert.Equal(t, "c1", c.ID())
	assert.Equal(t, task.Status_CREATED, c.Status())
	assert.Equal(t, []string{"c1"}, rt.created)
	// the spec was rewritten in place: cgroup nested under the sandbox, and
	// the pod netns pinned.
	assert.Equal(t, "/kata/sb/c1", p.Spec.Linux.CgroupsPath)
	assert.Equal(t, "/run/netns/pod", p.Spec.Linux.Namespaces[0].Path)
	// tracked and retrievable.
	assert.Same(t, c, m.Get("c1"))
}

func TestManagerCreateValidation(t *testing.T) {
	m := newTestManager(&fakeRuntime{})
	base := validParams(t)

	t.Run("empty id", func(t *testing.T) {
		p := base
		p.ID = ""
		_, err := m.Create(context.Background(), p)
		assert.Error(t, err)
	})
	t.Run("nil spec", func(t *testing.T) {
		p := base
		p.Spec = nil
		_, err := m.Create(context.Background(), p)
		assert.Error(t, err)
	})
	t.Run("empty netns", func(t *testing.T) {
		p := base
		p.NetnsPath = ""
		_, err := m.Create(context.Background(), p)
		assert.Error(t, err)
	})
}

func TestManagerCreateRuntimeError(t *testing.T) {
	rt := &fakeRuntime{createErr: errors.New("boom")}
	m := newTestManager(rt)
	_, err := m.Create(context.Background(), validParams(t))
	require.Error(t, err)
	assert.Nil(t, m.Get("c1"), "failed create must not be tracked")
}

func TestManagerGetUnknown(t *testing.T) {
	m := newTestManager(&fakeRuntime{})
	assert.Nil(t, m.Get("nope"))
}

// A nil manager (a service built directly in tests, bypassing New) must behave
// as "host routing disabled" rather than panic.
func TestNilManagerSafe(t *testing.T) {
	var m *Manager
	assert.False(t, m.Enabled())
	assert.Nil(t, m.Get("x"))
	assert.NotPanics(t, func() { m.Remove("x") })
}

func TestManagerRemove(t *testing.T) {
	rt := &fakeRuntime{}
	m := newTestManager(rt)
	_, err := m.Create(context.Background(), validParams(t))
	require.NoError(t, err)
	m.Remove("c1")
	assert.Nil(t, m.Get("c1"))
}

func TestContainerStart(t *testing.T) {
	rt := &fakeRuntime{state: &runc.Container{Pid: 4321, Status: "running"}}
	m := newTestManager(rt)
	c, err := m.Create(context.Background(), validParams(t))
	require.NoError(t, err)

	require.NoError(t, c.Start(context.Background()))
	assert.Equal(t, []string{"c1"}, rt.started)
	assert.Equal(t, task.Status_RUNNING, c.Status())
	assert.Equal(t, 4321, c.Pid())
}

func TestContainerKillIdempotentWhenStopped(t *testing.T) {
	rt := &fakeRuntime{}
	c := &Container{id: "c1", rt: rt, status: task.Status_STOPPED}

	require.NoError(t, c.Kill(context.Background(), uint32(9), false)) // SIGKILL
	require.NoError(t, c.Kill(context.Background(), uint32(15), false)) // SIGTERM
	assert.Empty(t, rt.killed, "stop signals to a stopped container must be no-ops")
}

func TestContainerKillRunning(t *testing.T) {
	rt := &fakeRuntime{}
	c := &Container{id: "c1", rt: rt, status: task.Status_RUNNING}

	require.NoError(t, c.Kill(context.Background(), uint32(9), true))
	require.Len(t, rt.killed, 1)
	assert.Equal(t, killCall{"c1", 9, true}, rt.killed[0])
}

func TestContainerKillNonTermSignalAlwaysDelivered(t *testing.T) {
	rt := &fakeRuntime{}
	c := &Container{id: "c1", rt: rt, status: task.Status_STOPPED}

	require.NoError(t, c.Kill(context.Background(), uint32(1), false)) // SIGHUP
	require.Len(t, rt.killed, 1, "non-terminating signals are delivered even when stopped")
}

func TestContainerDelete(t *testing.T) {
	rt := &fakeRuntime{}
	c := &Container{id: "c1", rt: rt}
	require.NoError(t, c.Delete(context.Background()))
	assert.Equal(t, []string{"c1"}, rt.deleted)
}

func TestContainerState(t *testing.T) {
	rt := &fakeRuntime{state: &runc.Container{Pid: 99, Status: "stopped"}}
	c := &Container{id: "c1", rt: rt}

	st, err := c.State(context.Background())
	require.NoError(t, err)
	assert.Equal(t, task.Status_STOPPED, st)
	assert.Equal(t, task.Status_STOPPED, c.Status())
	assert.Equal(t, 99, c.Pid())
}

func TestRuncStatusToTask(t *testing.T) {
	cases := map[string]task.Status{
		"created": task.Status_CREATED,
		"running": task.Status_RUNNING,
		"paused":  task.Status_PAUSED,
		"pausing": task.Status_PAUSING,
		"stopped": task.Status_STOPPED,
		"weird":   task.Status_UNKNOWN,
	}
	for in, want := range cases {
		assert.Equal(t, want, runcStatusToTask(in), in)
	}
}
