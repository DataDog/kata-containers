// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

package hostsidecar

// DefaultRuntimePath is the OCI runtime invoked for host sidecars when the
// configuration does not specify one. Resolved against $PATH by go-runc.
const DefaultRuntimePath = "runc"

// defaultRoot is the runc state root for host sidecars. It is kept separate
// from the system runc root so host-sidecar state never collides with other
// runc-managed containers on the node.
const defaultRoot = "/run/kata-hostsidecar"

// Config controls host-sidecar routing. It is populated from the Kata
// runtime configuration (configuration.toml) and is inert unless Enabled.
type Config struct {
	// Enabled turns host-sidecar routing on. When false, IsHostSidecar
	// matches are ignored and every container runs in the VM as usual.
	Enabled bool

	// RuntimePath is the OCI runtime binary used for host sidecars.
	RuntimePath string

	// Root is the OCI runtime state directory for host sidecars.
	Root string

	// SystemdCgroup selects the systemd cgroup driver for host sidecars,
	// matching the node's cgroup driver.
	SystemdCgroup bool
}

// withDefaults returns a copy of c with empty fields filled in.
func (c Config) withDefaults() Config {
	if c.RuntimePath == "" {
		c.RuntimePath = DefaultRuntimePath
	}
	if c.Root == "" {
		c.Root = defaultRoot
	}
	return c
}
