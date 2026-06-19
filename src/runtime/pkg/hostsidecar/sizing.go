// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

package hostsidecar

import (
	"strconv"

	specs "github.com/opencontainers/runtime-spec/specs-go"
	"github.com/sirupsen/logrus"
)

var sizingLog = logrus.WithField("source", "hostsidecar/sizing")

// HostSidecarMemBytesKey is the pod annotation that carries the total memory
// (in bytes) reserved for all host-sidecar containers combined.  The shim
// subtracts this from the VM memory allocation so the guest VM is not
// over-provisioned for resources that run on the host.
//
// The value must be a non-negative decimal integer, e.g. "134217728" (128 MiB).
// It is ignored when zero or absent.  Use the same annotation namespace as
// HostSidecarContainersKey so the containerd pod_annotations allowlist and
// Kata enable_annotations entries are shared.
const HostSidecarMemBytesKey = "io.katacontainers.host-sidecar-mem-bytes"

// HostSidecarCPUQuotaKey and HostSidecarCPUPeriodKey are the pod annotations
// for the total CPU quota (µs per period) and period (µs) reserved for all
// host-sidecar containers combined.  They are interpreted the same way as
// io.kubernetes.cri.sandbox-cpu-quota and sandbox-cpu-period.
const (
	HostSidecarCPUQuotaKey  = "io.katacontainers.host-sidecar-cpu-quota"
	HostSidecarCPUPeriodKey = "io.katacontainers.host-sidecar-cpu-period"
)

// SubtractFromSandboxSizing returns VM resource allocations with the
// host-sidecar portion removed.  It is a no-op when:
//   - spec is nil or carries no host-sidecar sizing annotations, or
//   - the upstream sizing already returned zeros (meaning "use hypervisor
//     default"), since we cannot subtract from an unknown total.
//
// The returned memory is floored at zero; a zero result tells the hypervisor
// to use its built-in default, which is safe because a zero input already had
// that meaning.
func SubtractFromSandboxSizing(spec *specs.Spec, cpuIn float32, memMBIn uint32) (cpu float32, memMB uint32) {
	if spec == nil || spec.Annotations == nil {
		return cpuIn, memMBIn
	}

	cpu = cpuIn
	memMB = memMBIn

	if raw, ok := spec.Annotations[HostSidecarMemBytesKey]; ok && raw != "" && memMBIn > 0 {
		bytes, err := strconv.ParseInt(raw, 10, 64)
		if err != nil || bytes < 0 {
			sizingLog.Warnf("sandbox-sizing: ignoring malformed %s=%q: %v", HostSidecarMemBytesKey, raw, err)
		} else {
			subtractMB := uint32(bytes / 1024 / 1024)
			if subtractMB >= memMB {
				sizingLog.Warnf("sandbox-sizing: host-sidecar mem (%d MB) >= sandbox total (%d MB); clamping VM to 0", subtractMB, memMB)
				memMB = 0
			} else {
				memMB -= subtractMB
				sizingLog.Debugf("sandbox-sizing: subtracted %d MB for host sidecars; VM gets %d MB", subtractMB, memMB)
			}
		}
	}

	if cpuIn > 0 {
		quotaRaw := spec.Annotations[HostSidecarCPUQuotaKey]
		periodRaw := spec.Annotations[HostSidecarCPUPeriodKey]
		if quotaRaw != "" && periodRaw != "" {
			quota, qErr := strconv.ParseInt(quotaRaw, 10, 64)
			period, pErr := strconv.ParseUint(periodRaw, 10, 64)
			if qErr != nil || pErr != nil || period == 0 {
				sizingLog.Warnf("sandbox-sizing: ignoring malformed host-sidecar CPU annotations quota=%q period=%q", quotaRaw, periodRaw)
			} else {
				sidecarlCPU := float32(quota) / float32(period)
				if sidecarlCPU >= cpu {
					sizingLog.Warnf("sandbox-sizing: host-sidecar CPU (%.3f) >= sandbox total (%.3f); clamping VM CPU to 0", sidecarlCPU, cpu)
					cpu = 0
				} else {
					cpu -= sidecarlCPU
					sizingLog.Debugf("sandbox-sizing: subtracted %.3f CPU for host sidecars; VM gets %.3f", sidecarlCPU, cpu)
				}
			}
		}
	}

	return cpu, memMB
}
