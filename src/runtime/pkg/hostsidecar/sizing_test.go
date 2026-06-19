// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

package hostsidecar

import (
	"fmt"
	"testing"

	specs "github.com/opencontainers/runtime-spec/specs-go"
	"github.com/stretchr/testify/assert"
)

func sizingAnnots(kv ...string) map[string]string {
	m := make(map[string]string)
	for i := 0; i+1 < len(kv); i += 2 {
		m[kv[i]] = kv[i+1]
	}
	return m
}

func specWithAnnots(a map[string]string) *specs.Spec {
	return &specs.Spec{Annotations: a}
}

func TestSubtractFromSandboxSizing(t *testing.T) {
	tests := []struct {
		name      string
		spec      *specs.Spec
		cpuIn     float32
		memMBIn   uint32
		wantCPU   float32
		wantMemMB uint32
	}{
		{
			name:      "nil spec is no-op",
			spec:      nil,
			cpuIn:     2, memMBIn: 512,
			wantCPU: 2, wantMemMB: 512,
		},
		{
			name:      "no annotations is no-op",
			spec:      &specs.Spec{},
			cpuIn:     2, memMBIn: 512,
			wantCPU: 2, wantMemMB: 512,
		},
		{
			name:      "absent mem annotation is no-op",
			spec:      specWithAnnots(sizingAnnots()),
			cpuIn:     2, memMBIn: 512,
			wantCPU: 2, wantMemMB: 512,
		},
		{
			name: "zero upstream sizing (unknown total) skips mem subtraction",
			spec: specWithAnnots(sizingAnnots(HostSidecarMemBytesKey, fmt.Sprintf("%d", 64*1024*1024))),
			cpuIn: 0, memMBIn: 0,
			wantCPU: 0, wantMemMB: 0,
		},
		{
			name: "subtract 64 MB from 512 MB",
			spec: specWithAnnots(sizingAnnots(HostSidecarMemBytesKey, fmt.Sprintf("%d", 64*1024*1024))),
			cpuIn: 2, memMBIn: 512,
			wantCPU: 2, wantMemMB: 448,
		},
		{
			name: "subtract 128 MB from 128 MB clamps to 0",
			spec: specWithAnnots(sizingAnnots(HostSidecarMemBytesKey, fmt.Sprintf("%d", 128*1024*1024))),
			cpuIn: 2, memMBIn: 128,
			wantCPU: 2, wantMemMB: 0,
		},
		{
			name: "sidecar mem larger than sandbox total clamps to 0",
			spec: specWithAnnots(sizingAnnots(HostSidecarMemBytesKey, fmt.Sprintf("%d", 512*1024*1024))),
			cpuIn: 2, memMBIn: 128,
			wantCPU: 2, wantMemMB: 0,
		},
		{
			name:      "malformed mem annotation is ignored",
			spec:      specWithAnnots(sizingAnnots(HostSidecarMemBytesKey, "notanumber")),
			cpuIn:     2, memMBIn: 512,
			wantCPU: 2, wantMemMB: 512,
		},
		{
			name:      "negative mem annotation is ignored",
			spec:      specWithAnnots(sizingAnnots(HostSidecarMemBytesKey, "-1")),
			cpuIn:     2, memMBIn: 512,
			wantCPU: 2, wantMemMB: 512,
		},
		{
			name: "subtract CPU quota/period",
			spec: specWithAnnots(sizingAnnots(
				HostSidecarCPUQuotaKey, "100000",
				HostSidecarCPUPeriodKey, "100000",
			)),
			cpuIn: 4, memMBIn: 512,
			wantCPU: 3, wantMemMB: 512,
		},
		{
			name: "CPU subtraction clamped to 0",
			spec: specWithAnnots(sizingAnnots(
				HostSidecarCPUQuotaKey, "800000",
				HostSidecarCPUPeriodKey, "100000",
			)),
			cpuIn: 2, memMBIn: 512,
			wantCPU: 0, wantMemMB: 512,
		},
		{
			name: "CPU annotations ignored when upstream CPU is 0",
			spec: specWithAnnots(sizingAnnots(
				HostSidecarCPUQuotaKey, "100000",
				HostSidecarCPUPeriodKey, "100000",
			)),
			cpuIn: 0, memMBIn: 512,
			wantCPU: 0, wantMemMB: 512,
		},
		{
			name: "malformed CPU period is ignored",
			spec: specWithAnnots(sizingAnnots(
				HostSidecarCPUQuotaKey, "100000",
				HostSidecarCPUPeriodKey, "bad",
			)),
			cpuIn: 4, memMBIn: 512,
			wantCPU: 4, wantMemMB: 512,
		},
		{
			name: "zero CPU period is ignored",
			spec: specWithAnnots(sizingAnnots(
				HostSidecarCPUQuotaKey, "100000",
				HostSidecarCPUPeriodKey, "0",
			)),
			cpuIn: 4, memMBIn: 512,
			wantCPU: 4, wantMemMB: 512,
		},
		{
			name: "both mem and CPU subtracted",
			spec: specWithAnnots(sizingAnnots(
				HostSidecarMemBytesKey,  fmt.Sprintf("%d", 64*1024*1024),
				HostSidecarCPUQuotaKey,  "100000",
				HostSidecarCPUPeriodKey, "100000",
			)),
			cpuIn: 4, memMBIn: 512,
			wantCPU: 3, wantMemMB: 448,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			gotCPU, gotMem := SubtractFromSandboxSizing(tt.spec, tt.cpuIn, tt.memMBIn)
			assert.Equal(t, tt.wantCPU, gotCPU, "cpu")
			assert.Equal(t, tt.wantMemMB, gotMem, "memMB")
		})
	}
}
