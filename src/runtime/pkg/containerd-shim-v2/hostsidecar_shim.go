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
	"errors"
	"fmt"
	"os"
	"path"
	"time"

	cgroupsv1 "github.com/containerd/cgroups/stats/v1"
	cgroupsv2 "github.com/containerd/cgroups/v2/stats"
	eventstypes "github.com/containerd/containerd/api/events"
	taskAPI "github.com/containerd/containerd/api/runtime/task/v2"
	"github.com/containerd/containerd/api/types/task"
	"github.com/containerd/containerd/mount"
	"github.com/containerd/containerd/protobuf"
	runc "github.com/containerd/go-runc"
	specs "github.com/opencontainers/runtime-spec/specs-go"
	"golang.org/x/sys/unix"
	anypb "google.golang.org/protobuf/types/known/anypb"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/kata-containers/kata-containers/src/runtime/pkg/hostsidecar"
	resCtrl "github.com/kata-containers/kata-containers/src/runtime/pkg/resourcecontrol"
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
//
// When containerd supplies non-empty stdout/stderr FIFO paths, a pipe IO is
// created so the shim can forward the sidecar's output to the log driver. The
// pipe read ends are stored on the hostsidecar.Container and retrieved by
// startHostContainer to launch the ioCopy goroutines.
func createHostContainer(ctx context.Context, s *service, r *taskAPI.CreateTaskRequest, spec *specs.Spec, bundlePath string) error {
	var pio runc.IO
	if r.Stdout != "" || r.Stderr != "" {
		var err error
		pio, err = runc.NewPipeIO(0, 0, func(o *runc.IOOption) {
			o.OpenStdin = r.Stdin != ""
		})
		if err != nil {
			return fmt.Errorf("host sidecar %s: pipe io: %w", r.ID, err)
		}
	}
	_, err := s.hostMgr.Create(ctx, hostsidecar.CreateParams{
		ID:        r.ID,
		SandboxID: s.sandbox.ID(),
		Bundle:    bundlePath,
		Spec:      spec,
		NetnsPath: s.sandbox.GetNetNs(),
		IO:        pio,
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

// startHostContainer starts a host sidecar, marks the shim container running,
// and wires up stdio forwarding when containerd requested log capture.
//
// If the container was created with a pipe IO (r.Stdout/r.Stderr non-empty),
// ioCopy goroutines are launched to stream the sidecar's output to
// containerd's log driver. exitIOch is closed by ioCopy when all data has been
// flushed — matching the in-VM container lifecycle so Wait behaves correctly.
// When no log capture was requested the channels are closed immediately, as
// before.
func startHostContainer(ctx context.Context, s *service, c *container, hc *hostsidecar.Container) error {
	if err := hc.Start(ctx); err != nil {
		return err
	}
	c.status = task.Status_RUNNING

	if hc.StdoutPipe() != nil || hc.StderrPipe() != nil {
		tty, err := newTtyIO(ctx, s.namespace, c.id, c.stdin, c.stdout, c.stderr, c.terminal)
		if err != nil {
			return fmt.Errorf("host sidecar %s: ttyIO: %w", c.id, err)
		}
		c.ttyio = tty
		c.stdinPipe = hc.StdinPipe()
		go func() {
			ioCopy(shimLog.WithField("container", c.id), c.exitIOch, c.stdinCloser, tty, hc.StdinPipe(), hc.StdoutPipe(), hc.StderrPipe())
			hc.ClosePipes()
		}()
	} else {
		close(c.exitIOch)
		close(c.stdinCloser)
	}
	return nil
}

// startHostExec runs an additional process inside a host sidecar via runc exec.
// It mirrors startExec for the VM path: creates pipe IO, wires ioCopy goroutines,
// runs runc exec in a goroutine, and drives the exec's exitCh when done.
//
// TTY execs (Terminal: true) are not yet supported — runc exec with a terminal
// requires a console socket, which is not wired here.
func startHostExec(ctx context.Context, s *service, c *container, hc *hostsidecar.Container, execID string) (*exec, error) {
	execs, err := c.getExec(execID)
	if err != nil {
		return nil, err
	}

	if execs.spec == nil {
		execs.exitCh <- exitCode255
		return nil, fmt.Errorf("host sidecar exec %s/%s: nil process spec", c.id, execID)
	}
	if execs.tty.terminal {
		execs.exitCh <- exitCode255
		return nil, fmt.Errorf("host sidecar exec %s/%s: TTY exec not yet supported", c.id, execID)
	}

	pio, err := runc.NewPipeIO(0, 0, func(o *runc.IOOption) {
		o.OpenStdin = execs.tty.stdin != ""
	})
	if err != nil {
		execs.exitCh <- exitCode255
		return nil, fmt.Errorf("host sidecar exec %s/%s: pipe io: %w", c.id, execID, err)
	}

	tty, err := newTtyIO(ctx, s.namespace, execID, execs.tty.stdin, execs.tty.stdout, execs.tty.stderr, false)
	if err != nil {
		_ = pio.Close()
		execs.exitCh <- exitCode255
		return nil, fmt.Errorf("host sidecar exec %s/%s: ttyIO: %w", c.id, execID, err)
	}
	execs.ttyio = tty
	execs.stdinPipe = pio.Stdin()
	execs.status = task.Status_RUNNING
	execs.id = execID

	// ioCopy bridges containerd FIFOs ↔ the exec process's pipe ends.
	// It closes exitIOch when all output has been drained.
	go func() {
		ioCopy(
			shimLog.WithField("container", c.id).WithField("exec", execID),
			execs.exitIOch, execs.stdinCloser,
			tty, pio.Stdin(), pio.Stdout(), pio.Stderr(),
		)
		_ = pio.Close()
	}()

	// Run runc exec; record exit once IO is fully drained.
	go func() {
		execErr := hc.Exec(ctx, *execs.spec, &runc.ExecOpts{IO: pio})

		// Wait for ioCopy to drain all remaining output before recording exit,
		// so that State() and Wait() only unblock after the last byte is written.
		<-execs.exitIOch

		exitCode := uint32(0)
		var exitErr *runc.ExitError
		if errors.As(execErr, &exitErr) {
			exitCode = uint32(exitErr.Status)
		} else if execErr != nil {
			exitCode = exitCode255
		}

		timeStamp := time.Now()
		s.mu.Lock()
		execs.status = task.Status_STOPPED
		execs.exitCode = int32(exitCode)
		execs.exitTime = timeStamp
		execs.exitCh <- exitCode
		s.mu.Unlock()

		go cReap(s, int(exitCode), c.id, execID, timeStamp)
	}()

	return execs, nil
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

// marshalHostSidecarStats converts runc cgroup statistics for a host sidecar
// into the same anypb.Any wire format that marshalMetrics produces for in-VM
// containers, so Stats() returns a consistent type to containerd / cAdvisor.
func marshalHostSidecarStats(ctx context.Context, hc *hostsidecar.Container) (*anypb.Any, error) {
	st, err := hc.Stats(ctx)
	if err != nil {
		return nil, err
	}

	isCgroupV1, err := resCtrl.IsCgroupV1()
	if err != nil {
		return nil, err
	}

	var metrics interface{}
	if isCgroupV1 {
		metrics = runcStatsToV1(st)
	} else {
		metrics = runcStatsToV2(st)
	}
	return protobuf.MarshalAnyToProto(metrics)
}

func runcStatsToV1(st *runc.Stats) *cgroupsv1.Metrics {
	m := &cgroupsv1.Metrics{
		Pids: &cgroupsv1.PidsStat{
			Current: st.Pids.Current,
			Limit:   st.Pids.Limit,
		},
		CPU: &cgroupsv1.CPUStat{
			Usage: &cgroupsv1.CPUUsage{
				Total:  st.Cpu.Usage.Total,
				Kernel: st.Cpu.Usage.Kernel,
				User:   st.Cpu.Usage.User,
				PerCPU: append([]uint64(nil), st.Cpu.Usage.Percpu...),
			},
			Throttling: &cgroupsv1.Throttle{
				Periods:          st.Cpu.Throttling.Periods,
				ThrottledPeriods: st.Cpu.Throttling.ThrottledPeriods,
				ThrottledTime:    st.Cpu.Throttling.ThrottledTime,
			},
		},
		Memory: &cgroupsv1.MemoryStat{
			Cache: st.Memory.Cache,
			Usage: &cgroupsv1.MemoryEntry{
				Limit:   st.Memory.Usage.Limit,
				Usage:   st.Memory.Usage.Usage,
				Max:     st.Memory.Usage.Max,
				Failcnt: st.Memory.Usage.Failcnt,
			},
			Swap: &cgroupsv1.MemoryEntry{
				Limit:   st.Memory.Swap.Limit,
				Usage:   st.Memory.Swap.Usage,
				Max:     st.Memory.Swap.Max,
				Failcnt: st.Memory.Swap.Failcnt,
			},
			Kernel: &cgroupsv1.MemoryEntry{
				Limit:   st.Memory.Kernel.Limit,
				Usage:   st.Memory.Kernel.Usage,
				Max:     st.Memory.Kernel.Max,
				Failcnt: st.Memory.Kernel.Failcnt,
			},
			KernelTCP: &cgroupsv1.MemoryEntry{
				Limit:   st.Memory.KernelTCP.Limit,
				Usage:   st.Memory.KernelTCP.Usage,
				Max:     st.Memory.KernelTCP.Max,
				Failcnt: st.Memory.KernelTCP.Failcnt,
			},
			RSS:        st.Memory.Raw["rss"],
			MappedFile: st.Memory.Raw["mapped_file"],
		},
		Blkio: &cgroupsv1.BlkIOStat{
			IoServiceBytesRecursive: runcBlkioToV1(st.Blkio.IoServiceBytesRecursive),
			IoServicedRecursive:     runcBlkioToV1(st.Blkio.IoServicedRecursive),
			IoQueuedRecursive:       runcBlkioToV1(st.Blkio.IoQueuedRecursive),
			SectorsRecursive:        runcBlkioToV1(st.Blkio.SectorsRecursive),
			IoServiceTimeRecursive:  runcBlkioToV1(st.Blkio.IoServiceTimeRecursive),
			IoWaitTimeRecursive:     runcBlkioToV1(st.Blkio.IoWaitTimeRecursive),
			IoMergedRecursive:       runcBlkioToV1(st.Blkio.IoMergedRecursive),
			IoTimeRecursive:         runcBlkioToV1(st.Blkio.IoTimeRecursive),
		},
	}
	for k, v := range st.Hugetlb {
		m.Hugetlb = append(m.Hugetlb, &cgroupsv1.HugetlbStat{
			Usage:    v.Usage,
			Max:      v.Max,
			Failcnt:  v.Failcnt,
			Pagesize: k,
		})
	}
	return m
}

func runcBlkioToV1(entries []runc.BlkioEntry) []*cgroupsv1.BlkIOEntry {
	out := make([]*cgroupsv1.BlkIOEntry, len(entries))
	for i, e := range entries {
		out[i] = &cgroupsv1.BlkIOEntry{
			Op:    e.Op,
			Major: e.Major,
			Minor: e.Minor,
			Value: e.Value,
		}
	}
	return out
}

func runcStatsToV2(st *runc.Stats) *cgroupsv2.Metrics {
	m := &cgroupsv2.Metrics{
		Pids: &cgroupsv2.PidsStat{
			Current: st.Pids.Current,
			Limit:   st.Pids.Limit,
		},
		CPU: &cgroupsv2.CPUStat{
			UsageUsec:     st.Cpu.Usage.Total / 1000,
			UserUsec:      st.Cpu.Usage.Kernel / 1000,
			SystemUsec:    st.Cpu.Usage.User / 1000,
			NrPeriods:     st.Cpu.Throttling.Periods,
			NrThrottled:   st.Cpu.Throttling.ThrottledPeriods,
			ThrottledUsec: st.Cpu.Throttling.ThrottledTime / 1000,
		},
		Memory: &cgroupsv2.MemoryStat{
			Usage:      st.Memory.Usage.Usage,
			UsageLimit: st.Memory.Usage.Limit,
			SwapUsage:  st.Memory.Swap.Usage,
			SwapLimit:  st.Memory.Swap.Limit,
		},
		Io: runcBlkioToV2(st.Blkio.IoServiceBytesRecursive),
	}
	raw := st.Memory.Raw
	setIfPresent := func(dst *uint64, key string) {
		if v, ok := raw[key]; ok {
			*dst = v
		}
	}
	setIfPresent(&m.Memory.Anon, "anon")
	setIfPresent(&m.Memory.File, "file")
	setIfPresent(&m.Memory.KernelStack, "kernel_stack")
	setIfPresent(&m.Memory.Slab, "slab")
	setIfPresent(&m.Memory.Sock, "sock")
	setIfPresent(&m.Memory.Shmem, "shmem")
	setIfPresent(&m.Memory.FileMapped, "file_mapped")
	setIfPresent(&m.Memory.FileDirty, "file_dirty")
	setIfPresent(&m.Memory.FileWriteback, "file_writeback")
	setIfPresent(&m.Memory.AnonThp, "anon_thp")
	setIfPresent(&m.Memory.InactiveAnon, "inactive_anon")
	setIfPresent(&m.Memory.ActiveAnon, "active_anon")
	setIfPresent(&m.Memory.InactiveFile, "inactive_file")
	setIfPresent(&m.Memory.ActiveFile, "active_file")
	setIfPresent(&m.Memory.Unevictable, "unevictable")
	setIfPresent(&m.Memory.SlabReclaimable, "slab_reclaimable")
	setIfPresent(&m.Memory.SlabUnreclaimable, "slab_unreclaimable")
	setIfPresent(&m.Memory.Pgfault, "pgfault")
	setIfPresent(&m.Memory.Pgmajfault, "pgmajfault")
	for k, v := range st.Hugetlb {
		m.Hugetlb = append(m.Hugetlb, &cgroupsv2.HugeTlbStat{
			Current:  v.Usage,
			Max:      v.Max,
			Pagesize: k,
		})
	}
	return m
}

func runcBlkioToV2(entries []runc.BlkioEntry) *cgroupsv2.IOStat {
	stat := &cgroupsv2.IOStat{}
	if len(entries) == 0 {
		return stat
	}
	item := &cgroupsv2.IOEntry{}
	for _, e := range entries {
		item.Major = e.Major
		item.Minor = e.Minor
		switch e.Op {
		case "read":
			item.Rbytes = e.Value
		case "write":
			item.Wbytes = e.Value
		case "rios":
			item.Rios = e.Value
		case "wios":
			item.Wios = e.Value
		}
	}
	stat.Usage = append(stat.Usage, item)
	return stat
}
