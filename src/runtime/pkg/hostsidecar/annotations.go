// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

// Package hostsidecar implements routing of annotated "host sidecar"
// containers to an OCI runtime on the host, instead of into the Kata guest
// VM. It is a self-contained extension to the Kata shim: the only upstream
// touch points are a dispatch branch in the shim's container-create path and a
// per-RPC guard in the task service (see HACKING.md). All routing, lifecycle,
// and resource logic lives in this package so that the feature carries forward
// across upstream rebases with minimal conflict.
package hostsidecar

import (
	"strings"

	ctrAnnotations "github.com/containerd/containerd/pkg/cri/annotations"
	specs "github.com/opencontainers/runtime-spec/specs-go"
)

// HostSidecarContainersKey is the pod annotation that lists, by container
// name, the containers that must run on the host rather than inside the guest
// VM. Its value is a comma-separated list of Kubernetes container names, e.g.
// "egress-proxy,metrics-agent". It is set at the pod level and propagated into
// each container's OCI spec by the CRI runtime (containerd must allowlist it
// via pod_annotations, and Kata via enable_annotations).
const HostSidecarContainersKey = "io.katacontainers.host-sidecar-containers"

// criContainerNameKeys lists the CRI annotation keys that may carry a
// container's Kubernetes name. containerd is the primary target; the CRI-O key
// is included as a literal fallback so the same shim works under either CRI.
var criContainerNameKeys = []string{
	ctrAnnotations.ContainerName,        // "io.kubernetes.cri.container-name"
	"io.kubernetes.cri-o.ContainerName", // CRI-O equivalent
}

// containerName returns the container's Kubernetes name as set by the CRI
// runtime, or "" if no recognised annotation is present.
func containerName(spec *specs.Spec) string {
	if spec == nil {
		return ""
	}
	for _, key := range criContainerNameKeys {
		if name := spec.Annotations[key]; name != "" {
			return name
		}
	}
	return ""
}

// HostSidecarNames returns the set of container names the pod marked to run on
// the host, parsed from HostSidecarContainersKey. Empty and whitespace-only
// entries are dropped. It returns nil if the annotation is absent or yields no
// names.
func HostSidecarNames(spec *specs.Spec) []string {
	if spec == nil {
		return nil
	}
	raw := spec.Annotations[HostSidecarContainersKey]
	if raw == "" {
		return nil
	}
	var names []string
	for _, field := range strings.Split(raw, ",") {
		if name := strings.TrimSpace(field); name != "" {
			names = append(names, name)
		}
	}
	return names
}

// IsHostSidecar reports whether the container described by spec must run on the
// host. It is true exactly when the container's CRI name appears in the pod's
// host-sidecar list. A container whose name cannot be determined never matches,
// so absent or malformed annotations fail closed (container stays in the VM).
func IsHostSidecar(spec *specs.Spec) bool {
	name := containerName(spec)
	if name == "" {
		return false
	}
	for _, want := range HostSidecarNames(spec) {
		if want == name {
			return true
		}
	}
	return false
}
