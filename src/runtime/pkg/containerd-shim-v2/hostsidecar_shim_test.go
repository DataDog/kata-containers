// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

package containerdshim

import (
	"context"
	"os"
	"testing"

	cgroupsv1 "github.com/containerd/cgroups/stats/v1"
	cgroupsv2 "github.com/containerd/cgroups/v2/stats"
	taskAPI "github.com/containerd/containerd/api/runtime/task/v2"
	"github.com/containerd/containerd/namespaces"
	"github.com/containerd/containerd/protobuf"
	_ "github.com/containerd/containerd/runtime" // registers specs.Process with typeurl
	runc "github.com/containerd/go-runc"
	"github.com/containerd/typeurl/v2"
	specs "github.com/opencontainers/runtime-spec/specs-go"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/kata-containers/kata-containers/src/runtime/pkg/hostsidecar"
)

// shimFakeRuntime is a test-only OCIRuntime that satisfies hostsidecar.OCIRuntime
// without a real runc binary. It returns the scripted pid from every State call.
type shimFakeRuntime struct {
	pid   int
	stats *runc.Stats
}

func (f *shimFakeRuntime) Create(_ context.Context, _, _ string, _ *runc.CreateOpts) error {
	return nil
}
func (f *shimFakeRuntime) Start(_ context.Context, _ string) error { return nil }
func (f *shimFakeRuntime) State(_ context.Context, _ string) (*runc.Container, error) {
	return &runc.Container{Pid: f.pid, Status: "running"}, nil
}
func (f *shimFakeRuntime) Kill(_ context.Context, _ string, _ int, _ *runc.KillOpts) error {
	return nil
}
func (f *shimFakeRuntime) Delete(_ context.Context, _ string, _ *runc.DeleteOpts) error {
	return nil
}
func (f *shimFakeRuntime) Pause(_ context.Context, _ string) error  { return nil }
func (f *shimFakeRuntime) Resume(_ context.Context, _ string) error { return nil }
func (f *shimFakeRuntime) Exec(_ context.Context, _ string, _ specs.Process, _ *runc.ExecOpts) error {
	return nil
}
func (f *shimFakeRuntime) Update(_ context.Context, _ string, _ *specs.LinuxResources) error {
	return nil
}
func (f *shimFakeRuntime) Stats(_ context.Context, _ string) (*runc.Stats, error) {
	if f.stats != nil {
		return f.stats, nil
	}
	return &runc.Stats{}, nil
}

// newServiceWithHostContainer creates a shim service that has a host-sidecar
// container pre-populated. The returned container has Pid == rt.pid (set via a
// single Container.State call, without starting the watch goroutine).
func newServiceWithHostContainer(t *testing.T, rt *shimFakeRuntime, containerID string) (*service, *hostsidecar.Container) {
	t.Helper()

	ctx := context.Background()
	mgr := hostsidecar.NewManagerWithRuntime(hostsidecar.Config{Enabled: true}, rt)

	bundleDir := t.TempDir()
	hc, err := mgr.Create(ctx, hostsidecar.CreateParams{
		ID:        containerID,
		SandboxID: "sb",
		Bundle:    bundleDir,
		Spec:      &specs.Spec{Linux: &specs.Linux{}},
		NetnsPath: "/run/netns/pod",
	})
	require.NoError(t, err)

	// Refresh the cached PID from the fake runtime without starting the watch goroutine.
	_, err = hc.State(ctx)
	require.NoError(t, err)

	s := &service{
		id:         "sb",
		pid:        uint32(os.Getpid()),
		hpid:       9999, // sentinel: must NOT appear in host-sidecar responses
		ctx:        ctx,
		containers: make(map[string]*container),
		events:     make(chan interface{}, chSize),
		ec:         make(chan exit, bufferSize),
		hostMgr:    mgr,
	}
	s.containers[containerID], err = newContainer(s, &taskAPI.CreateTaskRequest{ID: containerID}, "", nil, false)
	require.NoError(t, err)

	return s, hc
}

// ---------------------------------------------------------------------------
// Conversion function tests (runcStatsToV1 / runcStatsToV2)
// ---------------------------------------------------------------------------

func TestRuncBlkioToV1(t *testing.T) {
	entries := []runc.BlkioEntry{
		{Major: 8, Minor: 0, Op: "read", Value: 1024},
		{Major: 8, Minor: 0, Op: "write", Value: 2048},
	}
	got := runcBlkioToV1(entries)
	require.Len(t, got, 2)
	assert.Equal(t, uint64(8), got[0].Major)
	assert.Equal(t, "read", got[0].Op)
	assert.Equal(t, uint64(1024), got[0].Value)
	assert.Equal(t, "write", got[1].Op)
	assert.Equal(t, uint64(2048), got[1].Value)
}

func TestRuncBlkioToV1Empty(t *testing.T) {
	assert.Empty(t, runcBlkioToV1(nil))
	assert.Empty(t, runcBlkioToV1([]runc.BlkioEntry{}))
}

func TestRuncStatsToV1(t *testing.T) {
	st := &runc.Stats{
		Pids: runc.Pids{Current: 3, Limit: 100},
		Cpu: runc.Cpu{
			Usage: runc.CpuUsage{
				Total:  1_000_000,
				Kernel: 200_000,
				User:   800_000,
				Percpu: []uint64{500_000, 500_000},
			},
			Throttling: runc.Throttling{
				Periods:          10,
				ThrottledPeriods: 2,
				ThrottledTime:    50_000,
			},
		},
		Memory: runc.Memory{
			Cache: 4096,
			Usage: runc.MemoryEntry{Limit: 1 << 30, Usage: 512 << 20, Max: 600 << 20, Failcnt: 0},
			Swap:  runc.MemoryEntry{Limit: 2 << 30, Usage: 0},
			Raw:   map[string]uint64{"rss": 256 << 20, "mapped_file": 1 << 20},
		},
		Blkio: runc.Blkio{
			IoServiceBytesRecursive: []runc.BlkioEntry{{Major: 8, Minor: 0, Op: "read", Value: 4096}},
		},
		Hugetlb: map[string]runc.Hugetlb{
			"2MB": {Usage: 2 << 20, Max: 4 << 20, Failcnt: 0},
		},
	}

	m := runcStatsToV1(st)

	assert.Equal(t, uint64(3), m.Pids.Current)
	assert.Equal(t, uint64(100), m.Pids.Limit)

	assert.Equal(t, uint64(1_000_000), m.CPU.Usage.Total)
	assert.Equal(t, uint64(200_000), m.CPU.Usage.Kernel)
	assert.Equal(t, uint64(800_000), m.CPU.Usage.User)
	assert.Equal(t, []uint64{500_000, 500_000}, m.CPU.Usage.PerCPU)
	assert.Equal(t, uint64(10), m.CPU.Throttling.Periods)
	assert.Equal(t, uint64(2), m.CPU.Throttling.ThrottledPeriods)

	assert.Equal(t, uint64(4096), m.Memory.Cache)
	assert.Equal(t, uint64(1<<30), m.Memory.Usage.Limit)
	assert.Equal(t, uint64(512<<20), m.Memory.Usage.Usage)
	assert.Equal(t, uint64(256<<20), m.Memory.RSS)
	assert.Equal(t, uint64(1<<20), m.Memory.MappedFile)

	require.Len(t, m.Blkio.IoServiceBytesRecursive, 1)
	assert.Equal(t, "read", m.Blkio.IoServiceBytesRecursive[0].Op)
	assert.Equal(t, uint64(4096), m.Blkio.IoServiceBytesRecursive[0].Value)

	require.Len(t, m.Hugetlb, 1)
	assert.Equal(t, "2MB", m.Hugetlb[0].Pagesize)
	assert.Equal(t, uint64(2<<20), m.Hugetlb[0].Usage)
}

func TestRuncStatsToV2(t *testing.T) {
	st := &runc.Stats{
		Pids: runc.Pids{Current: 5, Limit: 50},
		Cpu: runc.Cpu{
			Usage: runc.CpuUsage{Total: 2_000_000, Kernel: 400_000, User: 1_600_000},
			Throttling: runc.Throttling{
				Periods:          20,
				ThrottledPeriods: 4,
				ThrottledTime:    100_000,
			},
		},
		Memory: runc.Memory{
			Usage: runc.MemoryEntry{Usage: 1 << 20, Limit: 1 << 30},
			Swap:  runc.MemoryEntry{Usage: 0, Limit: 2 << 30},
			Raw:   map[string]uint64{"anon": 512 << 10, "file": 256 << 10, "pgfault": 7},
		},
	}

	m := runcStatsToV2(st)

	assert.Equal(t, uint64(5), m.Pids.Current)
	assert.Equal(t, uint64(2_000_000/1000), m.CPU.UsageUsec)
	assert.Equal(t, uint64(20), m.CPU.NrPeriods)
	assert.Equal(t, uint64(4), m.CPU.NrThrottled)
	assert.Equal(t, uint64(1<<20), m.Memory.Usage)
	assert.Equal(t, uint64(1<<30), m.Memory.UsageLimit)
	assert.Equal(t, uint64(512<<10), m.Memory.Anon)
	assert.Equal(t, uint64(256<<10), m.Memory.File)
	assert.Equal(t, uint64(7), m.Memory.Pgfault)
}

// marshalHostSidecarStats uses resCtrl.IsCgroupV1() which reads from the real
// system; we only verify that it returns a non-nil Any with the correct type
// URL on Linux where cgroups are available.
func TestMarshalHostSidecarStats_TypeURL(t *testing.T) {
	fakeStats := &runc.Stats{
		Pids: runc.Pids{Current: 1},
	}
	rt := &shimFakeRuntime{stats: fakeStats}
	_, hc := newServiceWithHostContainer(t, rt, "c-stats")
	ctx := context.Background()

	data, err := marshalHostSidecarStats(ctx, hc)
	require.NoError(t, err)
	require.NotNil(t, data)

	// Must be one of the known cgroups metric types.
	typeURL := data.TypeUrl
	assert.True(t,
		typeURL == typeURLForV1() || typeURL == typeURLForV2(),
		"unexpected TypeUrl: %s", typeURL,
	)
}

func typeURLForV1() string {
	a, _ := protobuf.MarshalAnyToProto(&cgroupsv1.Metrics{})
	if a == nil {
		return ""
	}
	return a.TypeUrl
}

func typeURLForV2() string {
	a, _ := protobuf.MarshalAnyToProto(&cgroupsv2.Metrics{})
	if a == nil {
		return ""
	}
	return a.TypeUrl
}

// ---------------------------------------------------------------------------
// Service RPC guard tests
// ---------------------------------------------------------------------------

// TestExecRPCStoresExecForHostSidecar verifies that the Exec RPC no longer
// returns ErrNotImplemented for a host-sidecar container, but instead stores
// the exec in the container's execs map.
func TestExecRPCStoresExecForHostSidecar(t *testing.T) {
	rt := &shimFakeRuntime{pid: 42}
	s, _ := newServiceWithHostContainer(t, rt, testContainerID)

	ctx := namespaces.WithNamespace(context.Background(), "test")

	spec := &specs.Process{Args: []string{"/bin/sh", "-c", "echo hi"}}
	specAny, err := typeurl.MarshalAnyToProto(spec)
	require.NoError(t, err)

	req := &taskAPI.ExecProcessRequest{
		ID:     testContainerID,
		ExecID: "exec-1",
		Spec:   specAny,
	}
	_, err = s.Exec(ctx, req)
	require.NoError(t, err, "Exec RPC must not return ErrNotImplemented for host sidecar")

	c := s.containers[testContainerID]
	require.NotNil(t, c)
	e, err := c.getExec("exec-1")
	require.NoError(t, err)
	assert.NotNil(t, e.spec, "exec.spec must be populated by newExec")
}

// TestStateRPCUsesHostSidecarPid verifies that the State RPC returns the
// container's real host PID (from hc.Pid()), not the hypervisor PID (hpid).
func TestStateRPCUsesHostSidecarPid(t *testing.T) {
	const hostSidecarPid = 42

	rt := &shimFakeRuntime{pid: hostSidecarPid}
	s, _ := newServiceWithHostContainer(t, rt, testContainerID)

	ctx := namespaces.WithNamespace(context.Background(), "test")
	resp, err := s.State(ctx, &taskAPI.StateRequest{ID: testContainerID})
	require.NoError(t, err)

	assert.Equal(t, uint32(hostSidecarPid), resp.Pid,
		"State must return hc.Pid() (%d), not hpid (%d)", hostSidecarPid, s.hpid)
}

// TestPidsRPCUsesHostSidecarPid verifies that the Pids RPC returns the
// container's real host PID, not the hypervisor PID.
func TestPidsRPCUsesHostSidecarPid(t *testing.T) {
	const hostSidecarPid = 42

	rt := &shimFakeRuntime{pid: hostSidecarPid}
	s, _ := newServiceWithHostContainer(t, rt, testContainerID)

	ctx := namespaces.WithNamespace(context.Background(), "test")
	resp, err := s.Pids(ctx, &taskAPI.PidsRequest{ID: testContainerID})
	require.NoError(t, err)

	require.Len(t, resp.Processes, 1)
	assert.Equal(t, uint32(hostSidecarPid), resp.Processes[0].Pid,
		"Pids must return hc.Pid() (%d), not hpid (%d)", hostSidecarPid, s.hpid)
}
