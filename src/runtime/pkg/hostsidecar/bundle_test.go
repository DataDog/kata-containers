// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

package hostsidecar

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	specs "github.com/opencontainers/runtime-spec/specs-go"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestRewriteSpecForHost(t *testing.T) {
	t.Run("nil spec", func(t *testing.T) {
		assert.Error(t, rewriteSpecForHost(nil, "/netns", "/cg"))
	})

	t.Run("empty netns rejected", func(t *testing.T) {
		assert.Error(t, rewriteSpecForHost(&specs.Spec{}, "", "/cg"))
	})

	t.Run("sets cgroups path and appends netns", func(t *testing.T) {
		spec := &specs.Spec{Linux: &specs.Linux{}}
		require.NoError(t, rewriteSpecForHost(spec, "/run/netns/pod", "/kata/sb/c1"))
		assert.Equal(t, "/kata/sb/c1", spec.Linux.CgroupsPath)
		assert.Equal(t, []specs.LinuxNamespace{
			{Type: specs.NetworkNamespace, Path: "/run/netns/pod"},
		}, spec.Linux.Namespaces)
	})

	t.Run("allocates Linux section when absent", func(t *testing.T) {
		spec := &specs.Spec{}
		require.NoError(t, rewriteSpecForHost(spec, "/run/netns/pod", "/cg"))
		require.NotNil(t, spec.Linux)
		assert.Equal(t, "/run/netns/pod", spec.Linux.Namespaces[0].Path)
	})

	t.Run("replaces existing network namespace, preserves others", func(t *testing.T) {
		spec := &specs.Spec{Linux: &specs.Linux{Namespaces: []specs.LinuxNamespace{
			{Type: specs.PIDNamespace},
			{Type: specs.NetworkNamespace, Path: "/old"},
			{Type: specs.MountNamespace},
		}}}
		require.NoError(t, rewriteSpecForHost(spec, "/new", "/cg"))
		assert.Equal(t, []specs.LinuxNamespace{
			{Type: specs.PIDNamespace},
			{Type: specs.NetworkNamespace, Path: "/new"},
			{Type: specs.MountNamespace},
		}, spec.Linux.Namespaces)
	})

	t.Run("drops guest-only mounts", func(t *testing.T) {
		spec := &specs.Spec{
			Linux: &specs.Linux{},
			Mounts: []specs.Mount{
				{Destination: "/proc", Type: "proc", Source: "proc"},
				{Destination: "/shared", Type: "virtiofs", Source: "kataShared"},
				{Destination: "/kata", Source: "/run/kata-containers/foo"},
				{Destination: "/etc/hosts", Type: "bind", Source: "/var/lib/kubelet/hosts"},
			},
		}
		require.NoError(t, rewriteSpecForHost(spec, "/netns", "/cg"))
		assert.Equal(t, []specs.Mount{
			{Destination: "/proc", Type: "proc", Source: "proc"},
			{Destination: "/etc/hosts", Type: "bind", Source: "/var/lib/kubelet/hosts"},
		}, spec.Mounts)
	})
}

func TestWriteBundleConfig(t *testing.T) {
	dir := t.TempDir()
	spec := &specs.Spec{Version: "1.0.2", Linux: &specs.Linux{CgroupsPath: "/kata/sb/c1"}}
	require.NoError(t, writeBundleConfig(dir, spec))

	data, err := os.ReadFile(filepath.Join(dir, "config.json"))
	require.NoError(t, err)

	var got specs.Spec
	require.NoError(t, json.Unmarshal(data, &got))
	assert.Equal(t, "1.0.2", got.Version)
	assert.Equal(t, "/kata/sb/c1", got.Linux.CgroupsPath)
}

func TestWriteBundleConfigMissingDir(t *testing.T) {
	err := writeBundleConfig(filepath.Join(t.TempDir(), "does-not-exist"), &specs.Spec{})
	assert.Error(t, err)
}
