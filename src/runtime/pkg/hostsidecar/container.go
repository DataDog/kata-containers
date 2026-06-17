// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

package hostsidecar

import (
	"context"
	"io"
	"sync"
	"syscall"
	"time"

	"github.com/containerd/containerd/api/types/task"
	runc "github.com/containerd/go-runc"
	specs "github.com/opencontainers/runtime-spec/specs-go"
)

// defaultPollInterval is how often a started sidecar's status is polled to
// detect exit. The kata shim does not reap host processes (that would collide
// with how it reaps the hypervisor), so a short poll of the OCI runtime state
// is used instead of a SIGCHLD/subreaper handler.
const defaultPollInterval = 100 * time.Millisecond

// Container is a single host-side sidecar, managed through an OCI runtime
// (runc) on the host rather than the kata-agent inside the VM. It tracks just
// enough state for the shim's task service to report status and exit.
type Container struct {
	id     string
	bundle string
	rt     OCIRuntime

	pollInterval time.Duration
	// onExit, if set, is invoked once when the sidecar is observed to have
	// exited. The shim uses it to feed its own exit machinery (exitCh + the
	// TaskExit event) so State/Wait/Delete need no host-specific handling.
	onExit func(status uint32, at time.Time)
	// pipeIO, if non-nil, provides the container's stdio as pipe pairs so the
	// shim can forward stdout/stderr to containerd's log infrastructure. It is
	// set only when the caller requested log capture (i.e. r.Stdout != "").
	pipeIO runc.IO

	mu         sync.Mutex
	status     task.Status
	pid        int
	lastSignal uint32
	exitCode   uint32
	exitTime   time.Time
	exitCh     chan uint32
	exitOnce   sync.Once
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

// StdinPipe returns the write-end of the container's stdin pipe, or nil if
// no stdin capture was requested at create time.
func (c *Container) StdinPipe() io.WriteCloser {
	if c.pipeIO == nil {
		return nil
	}
	return c.pipeIO.Stdin()
}

// StdoutPipe returns the read-end of the container's stdout pipe, or nil if
// no stdout capture was requested at create time.
func (c *Container) StdoutPipe() io.ReadCloser {
	if c.pipeIO == nil {
		return nil
	}
	return c.pipeIO.Stdout()
}

// StderrPipe returns the read-end of the container's stderr pipe, or nil if
// no stderr capture was requested at create time.
func (c *Container) StderrPipe() io.ReadCloser {
	if c.pipeIO == nil {
		return nil
	}
	return c.pipeIO.Stderr()
}

// ClosePipes closes all pipe FDs. Called after ioCopy finishes to release
// the read ends that were drained.
func (c *Container) ClosePipes() {
	if c.pipeIO != nil {
		_ = c.pipeIO.Close()
	}
}

// Start starts the created container, refreshes its PID, and begins watching
// for exit. The watcher runs until the container stops and then reports the
// exit exactly once.
func (c *Container) Start(ctx context.Context) error {
	if err := c.rt.Start(ctx, c.id); err != nil {
		return err
	}
	c.mu.Lock()
	c.status = task.Status_RUNNING
	c.mu.Unlock()
	c.refreshPid(ctx)
	go c.watch()
	return nil
}

// ExitCh returns a channel that receives the exit code once the sidecar exits.
// It is buffered and refilled so multiple waiters each observe the code.
func (c *Container) ExitCh() <-chan uint32 { return c.exitCh }

// ExitedAt returns the time the sidecar exited (zero if still running).
func (c *Container) ExitedAt() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.exitTime
}

// ExitCode returns the sidecar's exit code (best-effort; see markExited).
func (c *Container) ExitCode() uint32 {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.exitCode
}

// watchMaxConsecutiveErrors is the number of consecutive runc-state errors
// that must occur before watch concludes the container is gone. A single
// transient error (e.g. lock contention during runc exec) must not be
// treated as container death.
const watchMaxConsecutiveErrors = 3

// watch polls the runtime until the container stops (or disappears), then
// records the exit once.
func (c *Container) watch() {
	interval := c.pollInterval
	if interval == 0 {
		interval = defaultPollInterval
	}
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	consecutiveErrors := 0
	for range ticker.C {
		st, err := c.rt.State(context.Background(), c.id)
		if err != nil || st == nil {
			consecutiveErrors++
			if consecutiveErrors >= watchMaxConsecutiveErrors {
				c.markExited(c.inferExitCode())
				return
			}
			continue
		}
		consecutiveErrors = 0
		if runcStatusToTask(st.Status) == task.Status_STOPPED {
			c.markExited(c.inferExitCode())
			return
		}
	}
}

// markExited records the terminal state and notifies waiters exactly once.
func (c *Container) markExited(code uint32) {
	c.exitOnce.Do(func() {
		c.mu.Lock()
		c.status = task.Status_STOPPED
		c.exitCode = code
		c.exitTime = time.Now()
		onExit := c.onExit
		c.mu.Unlock()

		// exitCh is buffered (cap 1) and refilled so concurrent waiters all
		// observe the code, mirroring the shim's own Wait semantics.
		select {
		case c.exitCh <- code:
		default:
		}
		if onExit != nil {
			onExit(code, c.exitTime)
		}
	})
}

// inferExitCode derives an exit code from the last signal delivered, since the
// OCI runtime does not expose the real status to a non-parent. A clean exit
// (no terminating signal observed) is reported as 0.
func (c *Container) inferExitCode() uint32 {
	c.mu.Lock()
	sig := c.lastSignal
	c.mu.Unlock()
	if sig == 0 {
		return 0
	}
	return 128 + sig
}

// Kill signals the container. Sending SIGKILL/SIGTERM to an already-stopped
// container is a no-op, mirroring the shim's idempotent stop semantics (the
// kubelet may call stop more than once).
func (c *Container) Kill(ctx context.Context, sig uint32, all bool) error {
	signum := syscall.Signal(sig)
	if (signum == syscall.SIGKILL || signum == syscall.SIGTERM) && c.Status() == task.Status_STOPPED {
		return nil
	}
	c.mu.Lock()
	c.lastSignal = sig
	c.mu.Unlock()
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

// Pause freezes the container's processes.
func (c *Container) Pause(ctx context.Context) error {
	if err := c.rt.Pause(ctx, c.id); err != nil {
		return err
	}
	c.mu.Lock()
	c.status = task.Status_PAUSED
	c.mu.Unlock()
	return nil
}

// Resume unfreezes the container's processes.
func (c *Container) Resume(ctx context.Context) error {
	if err := c.rt.Resume(ctx, c.id); err != nil {
		return err
	}
	c.mu.Lock()
	c.status = task.Status_RUNNING
	c.mu.Unlock()
	return nil
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
