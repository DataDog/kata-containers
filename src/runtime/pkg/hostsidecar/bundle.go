// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

package hostsidecar

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	specs "github.com/opencontainers/runtime-spec/specs-go"
)

// guestOnlyMountTypes are filesystem types that only resolve inside the guest
// VM; a host sidecar cannot mount them.
var guestOnlyMountTypes = []string{"virtiofs", "nydus", "kataShared"}

// guestOnlyMountSourcePrefixes are mount source prefixes specific to the Kata
// guest. Mounts under these paths are dropped for host sidecars.
var guestOnlyMountSourcePrefixes = []string{"/run/kata-containers"}

// rewriteSpecForHost adapts an OCI spec that was produced for an in-VM
// container so it can run on the host inside the pod network namespace:
//
//   - forces the container into the pod network namespace (netnsPath), giving
//     it the pod's network identity;
//   - sets the cgroups path so the OCI runtime accounts the sidecar under the
//     pod's host cgroup hierarchy rather than a default location;
//   - drops mounts that only resolve inside the guest VM (virtio-fs shares,
//     /run/kata-containers paths), which a host process cannot use.
//
// It mutates spec in place. netnsPath must be non-empty: a host sidecar that
// did not join the pod netns would have the wrong network identity.
func rewriteSpecForHost(spec *specs.Spec, netnsPath, cgroupsPath string) error {
	if spec == nil {
		return errors.New("nil OCI spec")
	}
	if netnsPath == "" {
		return errors.New("empty network namespace path")
	}
	if spec.Linux == nil {
		spec.Linux = &specs.Linux{}
	}

	spec.Linux.CgroupsPath = cgroupsPath
	setNetworkNamespace(spec.Linux, netnsPath)
	spec.Mounts = stripGuestOnlyMounts(spec.Mounts)
	return nil
}

// setNetworkNamespace pins the network namespace to path, replacing any
// existing network namespace entry or appending one if absent.
func setNetworkNamespace(l *specs.Linux, path string) {
	ns := specs.LinuxNamespace{Type: specs.NetworkNamespace, Path: path}
	for i := range l.Namespaces {
		if l.Namespaces[i].Type == specs.NetworkNamespace {
			l.Namespaces[i] = ns
			return
		}
	}
	l.Namespaces = append(l.Namespaces, ns)
}

// stripGuestOnlyMounts returns mounts with guest-only entries removed. It
// returns nil when no mounts remain so the result is stable for comparison.
func stripGuestOnlyMounts(mounts []specs.Mount) []specs.Mount {
	var kept []specs.Mount
	for _, m := range mounts {
		if isGuestOnlyMount(m) {
			continue
		}
		kept = append(kept, m)
	}
	return kept
}

func isGuestOnlyMount(m specs.Mount) bool {
	for _, t := range guestOnlyMountTypes {
		if m.Type == t {
			return true
		}
	}
	for _, p := range guestOnlyMountSourcePrefixes {
		if strings.HasPrefix(m.Source, p) {
			return true
		}
	}
	return false
}

// writeBundleConfig writes spec as config.json into bundleDir, which must
// already exist (it is the bundle containerd prepared, holding rootfs/). The
// OCI runtime reads the rewritten config from there.
func writeBundleConfig(bundleDir string, spec *specs.Spec) error {
	data, err := json.Marshal(spec)
	if err != nil {
		return fmt.Errorf("marshal OCI spec: %w", err)
	}
	configPath := filepath.Join(bundleDir, "config.json")
	if err := os.WriteFile(configPath, data, 0o600); err != nil {
		return fmt.Errorf("write %s: %w", configPath, err)
	}
	return nil
}
