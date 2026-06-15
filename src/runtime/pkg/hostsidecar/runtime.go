// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

package hostsidecar

import (
	"context"

	runc "github.com/containerd/go-runc"
	specs "github.com/opencontainers/runtime-spec/specs-go"
)

// OCIRuntime is the subset of an OCI runtime (runc/crun) that the host-sidecar
// manager drives. It mirrors the relevant *github.com/containerd/go-runc.Runc
// methods so that the concrete go-runc client satisfies it directly and the
// manager can be unit-tested against a fake. Keeping the surface narrow also
// limits how much of go-runc the rest of the package depends on.
type OCIRuntime interface {
	Create(ctx context.Context, id, bundle string, opts *runc.CreateOpts) error
	Start(ctx context.Context, id string) error
	State(ctx context.Context, id string) (*runc.Container, error)
	Kill(ctx context.Context, id string, sig int, opts *runc.KillOpts) error
	Delete(ctx context.Context, id string, opts *runc.DeleteOpts) error
	Exec(ctx context.Context, id string, spec specs.Process, opts *runc.ExecOpts) error
	Update(ctx context.Context, id string, resources *specs.LinuxResources) error
	Stats(ctx context.Context, id string) (*runc.Stats, error)
}

// the go-runc client must satisfy the interface the manager relies on.
var _ OCIRuntime = (*runc.Runc)(nil)

// newRuncRuntime builds a go-runc client from the resolved config.
func newRuncRuntime(cfg Config) *runc.Runc {
	return &runc.Runc{
		Command:       cfg.RuntimePath,
		Root:          cfg.Root,
		SystemdCgroup: cfg.SystemdCgroup,
		// Reap the runc process group so a dying shim does not leak it.
		Setpgid: true,
	}
}
