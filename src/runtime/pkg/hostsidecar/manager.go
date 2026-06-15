// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

package hostsidecar

import (
	"context"
	"fmt"
	"path/filepath"
	"sync"
	"time"

	"github.com/containerd/containerd/api/types/task"
	runc "github.com/containerd/go-runc"
	specs "github.com/opencontainers/runtime-spec/specs-go"
)

// Manager owns the host-side sidecar containers for one pod (one shim). It
// routes annotated containers to an OCI runtime on the host and tracks their
// lifecycle, leaving in-VM containers entirely to the existing Kata path.
type Manager struct {
	cfg Config
	rt  OCIRuntime

	mu         sync.Mutex
	containers map[string]*Container
}

// NewManager builds a Manager backed by a real go-runc runtime.
func NewManager(cfg Config) *Manager {
	cfg = cfg.withDefaults()
	return newManagerWithRuntime(cfg, newRuncRuntime(cfg))
}

// newManagerWithRuntime builds a Manager with an injected runtime, used in
// tests with a fake OCIRuntime.
func newManagerWithRuntime(cfg Config, rt OCIRuntime) *Manager {
	return &Manager{
		cfg:        cfg.withDefaults(),
		rt:         rt,
		containers: make(map[string]*Container),
	}
}

// Enabled reports whether host-sidecar routing is active. A nil manager (e.g.
// a service constructed directly in tests, bypassing New) is treated as
// disabled so callers need no nil checks.
func (m *Manager) Enabled() bool { return m != nil && m.cfg.Enabled }

// Get returns the host sidecar for id, or nil if id is not host-managed (or the
// manager is nil). A nil return is the signal the shim uses to fall back to the
// in-VM path.
func (m *Manager) Get(id string) *Container {
	if m == nil {
		return nil
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.containers[id]
}

// CreateParams carries everything needed to create a host sidecar. The shim
// supplies these from the CreateTaskRequest and the pod's network namespace.
type CreateParams struct {
	ID        string
	SandboxID string
	// Bundle is the OCI bundle directory containerd prepared (contains rootfs/).
	Bundle string
	// Spec is the container's OCI spec; it is rewritten for host execution.
	Spec *specs.Spec
	// NetnsPath is the pod network namespace the sidecar must join.
	NetnsPath string
	// IO wires the container's stdio; nil leaves it to the runtime default.
	IO runc.IO
	// OnExit, if set, is invoked once when the sidecar exits. The shim uses
	// it to drive its existing exit machinery.
	OnExit func(status uint32, at time.Time)
}

// Create rewrites the spec for host execution, writes the bundle config, and
// creates the container via the OCI runtime. The container is recorded and
// returned in the CREATED state.
func (m *Manager) Create(ctx context.Context, p CreateParams) (*Container, error) {
	if p.ID == "" {
		return nil, fmt.Errorf("host sidecar: empty container id")
	}
	if p.Spec == nil {
		return nil, fmt.Errorf("host sidecar %s: nil spec", p.ID)
	}

	cgPath := cgroupPath(p.SandboxID, p.ID)
	if err := rewriteSpecForHost(p.Spec, p.NetnsPath, cgPath); err != nil {
		return nil, fmt.Errorf("host sidecar %s: rewrite spec: %w", p.ID, err)
	}
	if err := writeBundleConfig(p.Bundle, p.Spec); err != nil {
		return nil, fmt.Errorf("host sidecar %s: %w", p.ID, err)
	}

	// IO must be non-nil. With a nil IO, go-runc captures "runc create"'s
	// combined output via a pipe that the container's init process inherits;
	// the read then blocks until the sidecar exits, hanging Create. NullIO
	// points the container's stdio at /dev/null so create returns promptly.
	// (Real log/stream wiring can replace this later.)
	io := p.IO
	if io == nil {
		nullIO, err := runc.NewNullIO()
		if err != nil {
			return nil, fmt.Errorf("host sidecar %s: null io: %w", p.ID, err)
		}
		io = nullIO
	}

	// Note: no Detach here. "runc create" already creates the container
	// detached (its init waits for "runc start"); --detach is only valid for
	// "runc run" and is rejected by "runc create".
	opts := &runc.CreateOpts{IO: io}
	if err := m.rt.Create(ctx, p.ID, p.Bundle, opts); err != nil {
		return nil, fmt.Errorf("host sidecar %s: runtime create: %w", p.ID, err)
	}

	c := &Container{
		id:     p.ID,
		bundle: p.Bundle,
		rt:     m.rt,
		status: task.Status_CREATED,
		onExit: p.OnExit,
		exitCh: make(chan uint32, 1),
	}
	m.mu.Lock()
	m.containers[p.ID] = c
	m.mu.Unlock()
	return c, nil
}

// Remove drops a container from the manager's tracking. Callers must have
// already deleted it from the runtime (see Container.Delete).
func (m *Manager) Remove(id string) {
	if m == nil {
		return
	}
	m.mu.Lock()
	delete(m.containers, id)
	m.mu.Unlock()
}

// cgroupPath returns the host cgroup path for a sidecar, nested under the pod's
// sandbox so the pod's aggregate resource limits apply.
func cgroupPath(sandboxID, id string) string {
	return filepath.Join("/kata", sandboxID, id)
}
