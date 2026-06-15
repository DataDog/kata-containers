// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

package hostsidecar

import (
	"context"
	"sync"
	"syscall"
	"time"

	"github.com/containerd/containerd/api/types/task"
	runc "github.com/containerd/go-runc"
	specs "github.com/opencontainers/runtime-spec/specs-go"
)

// Container is a single host-side sidecar, managed through an OCI runtime
// (runc) on the host rather than the kata-agent inside the VM. It tracks just
// enough state for the shim's task service to report status and exit.
type Container struct {
	id     string
	bundle string
	rt     OCIRuntime

	mu       sync.Mutex
	status   task.Status
	pid      int
	exitCode uint32
	exitTime time.Time
}

// ID returns the container ID.
func (c *Container) ID() string { return c.id }

// Bundle returns the OCI bundle directory.
func (c *Container) Bundle() string { return c.bundle }

// Status returns the last known task status.
func (c *Container) Status() task.Status {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.status
}

// Pid returns the sidecar's host PID, or 0 if not started.
func (c *Container) Pid() int {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.pid
}

// Start starts the created container and refreshes its PID from the runtime.
func (c *Container) Start(ctx context.Context) error {
	if err := c.rt.Start(ctx, c.id); err != nil {
		return err
	}
	c.mu.Lock()
	c.status = task.Status_RUNNING
	c.mu.Unlock()
	c.refreshPid(ctx)
	return nil
}

// Kill signals the container. Sending SIGKILL/SIGTERM to an already-stopped
// container is a no-op, mirroring the shim's idempotent stop semantics (the
// kubelet may call stop more than once).
func (c *Container) Kill(ctx context.Context, sig uint32, all bool) error {
	signum := syscall.Signal(sig)
	if (signum == syscall.SIGKILL || signum == syscall.SIGTERM) && c.Status() == task.Status_STOPPED {
		return nil
	}
	return c.rt.Kill(ctx, c.id, int(sig), &runc.KillOpts{All: all})
}

// Delete removes the container's runtime state. force ensures a running
// container is killed first, matching shim teardown where containerd may
// delete without a prior successful stop.
func (c *Container) Delete(ctx context.Context) error {
	return c.rt.Delete(ctx, c.id, &runc.DeleteOpts{Force: true})
}

// Exec runs an additional process inside the container.
func (c *Container) Exec(ctx context.Context, process specs.Process, opts *runc.ExecOpts) error {
	return c.rt.Exec(ctx, c.id, process, opts)
}

// Update applies new resource limits to the running container.
func (c *Container) Update(ctx context.Context, resources *specs.LinuxResources) error {
	return c.rt.Update(ctx, c.id, resources)
}

// Stats returns runtime resource statistics.
func (c *Container) Stats(ctx context.Context) (*runc.Stats, error) {
	return c.rt.Stats(ctx, c.id)
}

// State queries the runtime for the live status, updates the cached status and
// PID, and returns the mapped task status.
func (c *Container) State(ctx context.Context) (task.Status, error) {
	st, err := c.rt.State(ctx, c.id)
	if err != nil {
		return task.Status_UNKNOWN, err
	}
	status := runcStatusToTask(st.Status)
	c.mu.Lock()
	c.status = status
	if st.Pid != 0 {
		c.pid = st.Pid
	}
	c.mu.Unlock()
	return status, nil
}

// refreshPid best-effort updates the cached PID from the runtime.
func (c *Container) refreshPid(ctx context.Context) {
	st, err := c.rt.State(ctx, c.id)
	if err != nil || st == nil {
		return
	}
	c.mu.Lock()
	c.pid = st.Pid
	c.mu.Unlock()
}

// runcStatusToTask maps a runc state string to a containerd task status.
func runcStatusToTask(status string) task.Status {
	switch status {
	case "created":
		return task.Status_CREATED
	case "running":
		return task.Status_RUNNING
	case "paused":
		return task.Status_PAUSED
	case "pausing":
		return task.Status_PAUSING
	case "stopped":
		return task.Status_STOPPED
	default:
		return task.Status_UNKNOWN
	}
}
