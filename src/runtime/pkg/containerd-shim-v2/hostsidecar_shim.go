// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

// This file is the integration glue between the Kata shim and the
// self-contained pkg/hostsidecar package. Keeping it in its own file (rather
// than spreading logic through service.go/create.go) limits the upstream diff
// to a handful of one-line delegation guards, so the feature carries forward
// across upstream rebases with minimal conflict. See pkg/hostsidecar/HACKING.md.

package containerdshim

import (
	"context"
	"os"
	"path"
	"time"

	eventstypes "github.com/containerd/containerd/api/events"
	taskAPI "github.com/containerd/containerd/api/runtime/task/v2"
	"github.com/containerd/containerd/api/types/task"
	"github.com/containerd/containerd/mount"
	specs "github.com/opencontainers/runtime-spec/specs-go"
	"golang.org/x/sys/unix"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/kata-containers/kata-containers/src/runtime/pkg/hostsidecar"
)

// hostSidecarDisableEnv disables host-sidecar routing when set, regardless of
// pod annotations. Routing is otherwise gated by the pod annotation plus
// Kata's enable_annotations allowlist, so this is an operator kill switch.
const hostSidecarDisableEnv = "KATA_HOST_SIDECAR_DISABLE"

// newHostSidecarManager builds the per-pod host-sidecar manager owned by the
// shim service.
func newHostSidecarManager() *hostsidecar.Manager {
	return hostsidecar.NewManager(hostsidecar.Config{
		Enabled: os.Getenv(hostSidecarDisableEnv) == "",
	})
}

// createHostContainer routes an annotated container to an OCI runtime on the
// host (in the pod network namespace) instead of into the VM. The rootfs is
// already mounted at <bundlePath>/rootfs by the caller.
func createHostContainer(ctx context.Context, s *service, r *taskAPI.CreateTaskRequest, spec *specs.Spec, bundlePath string) error {
	_, err := s.hostMgr.Create(ctx, hostsidecar.CreateParams{
		ID:        r.ID,
		SandboxID: s.sandbox.ID(),
		Bundle:    bundlePath,
		Spec:      spec,
		NetnsPath: s.sandbox.GetNetNs(),
		OnExit:    s.hostSidecarOnExit(r.ID),
	})
	return err
}

// hostSidecarOnExit feeds a host sidecar's exit into the shim's existing exit
// machinery (the container's exitCh and a TaskExit event), so that State, Wait
// and Delete behave exactly as they do for in-VM containers and need no
// host-specific handling.
func (s *service) hostSidecarOnExit(id string) func(uint32, time.Time) {
	return func(status uint32, at time.Time) {
		s.mu.Lock()
		if c := s.containers[id]; c != nil {
			c.status = task.Status_STOPPED
			c.exit = status
			c.exitTime = at
			// exitCh is buffered (cap 1) and refilled by Wait; never block.
			select {
			case c.exitCh <- status:
			default:
			}
		}
		s.mu.Unlock()
		s.send(&eventstypes.TaskExit{
			ContainerID: id,
			ID:          id,
			Pid:         s.hpid,
			ExitStatus:  status,
			ExitedAt:    timestamppb.New(at),
		})
	}
}

// startHostContainer starts a host sidecar and marks the shim container
// running. Host sidecars have no VM IO streams, so the generic io/stdin
// channels are closed here to avoid blocking the shim's close paths.
func startHostContainer(ctx context.Context, c *container, hc *hostsidecar.Container) error {
	if err := hc.Start(ctx); err != nil {
		return err
	}
	c.status = task.Status_RUNNING
	close(c.exitIOch)
	close(c.stdinCloser)
	return nil
}

// deleteHostContainer stops and removes a host sidecar and unmounts its rootfs,
// mirroring deleteContainer for the in-VM path.
func deleteHostContainer(ctx context.Context, s *service, c *container, hc *hostsidecar.Container) error {
	if c.status != task.Status_STOPPED {
		if err := hc.Kill(ctx, uint32(unix.SIGKILL), true); err != nil {
			shimLog.WithError(err).Warn("failed to kill host sidecar before delete")
		}
	}
	if err := hc.Delete(ctx); err != nil {
		shimLog.WithError(err).Warn("failed to delete host sidecar")
	}
	s.hostMgr.Remove(c.id)

	if c.mounted {
		rootfs := path.Join(c.bundle, "rootfs")
		if err := mount.UnmountAll(rootfs, 0); err != nil {
			shimLog.WithError(err).Warn("failed to cleanup host sidecar rootfs mount")
		}
	}
	delete(s.containers, c.id)
	return nil
}
