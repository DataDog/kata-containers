// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

package hostsidecar

import (
	"testing"

	ctrAnnotations "github.com/containerd/containerd/pkg/cri/annotations"
	specs "github.com/opencontainers/runtime-spec/specs-go"
	"github.com/stretchr/testify/assert"
)

const crioContainerNameKey = "io.kubernetes.cri-o.ContainerName"

// specWith builds a spec carrying the given annotations. A nil map is allowed.
func specWith(anno map[string]string) *specs.Spec {
	return &specs.Spec{Annotations: anno}
}

func TestHostSidecarNames(t *testing.T) {
	tests := []struct {
		name string
		spec *specs.Spec
		want []string
	}{
		{"nil spec", nil, nil},
		{"nil annotations", specWith(nil), nil},
		{"absent key", specWith(map[string]string{"other": "x"}), nil},
		{"empty value", specWith(map[string]string{HostSidecarContainersKey: ""}), nil},
		{"single", specWith(map[string]string{HostSidecarContainersKey: "proxy"}), []string{"proxy"}},
		{"multiple", specWith(map[string]string{HostSidecarContainersKey: "proxy,agent"}), []string{"proxy", "agent"}},
		{"whitespace trimmed", specWith(map[string]string{HostSidecarContainersKey: " proxy , agent "}), []string{"proxy", "agent"}},
		{"empty entries dropped", specWith(map[string]string{HostSidecarContainersKey: "proxy,,agent,"}), []string{"proxy", "agent"}},
		{"only separators", specWith(map[string]string{HostSidecarContainersKey: " , , "}), nil},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.want, HostSidecarNames(tt.spec))
		})
	}
}

func TestContainerName(t *testing.T) {
	tests := []struct {
		name string
		spec *specs.Spec
		want string
	}{
		{"nil spec", nil, ""},
		{"nil annotations", specWith(nil), ""},
		{"absent", specWith(map[string]string{"other": "x"}), ""},
		{"containerd key", specWith(map[string]string{ctrAnnotations.ContainerName: "proxy"}), "proxy"},
		{"crio key", specWith(map[string]string{crioContainerNameKey: "proxy"}), "proxy"},
		{
			"containerd preferred over crio",
			specWith(map[string]string{ctrAnnotations.ContainerName: "ctrd", crioContainerNameKey: "crio"}),
			"ctrd",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.want, containerName(tt.spec))
		})
	}
}

func TestIsHostSidecar(t *testing.T) {
	tests := []struct {
		name string
		spec *specs.Spec
		want bool
	}{
		{"nil spec", nil, false},
		{"no annotations", specWith(nil), false},
		{
			"name in list",
			specWith(map[string]string{
				ctrAnnotations.ContainerName: "proxy",
				HostSidecarContainersKey:     "proxy,agent",
			}),
			true,
		},
		{
			"name not in list",
			specWith(map[string]string{
				ctrAnnotations.ContainerName: "workload",
				HostSidecarContainersKey:     "proxy,agent",
			}),
			false,
		},
		{
			"list present but name unknown (fail closed)",
			specWith(map[string]string{
				HostSidecarContainersKey: "proxy",
			}),
			false,
		},
		{
			"name present but no list",
			specWith(map[string]string{
				ctrAnnotations.ContainerName: "proxy",
			}),
			false,
		},
		{
			"whitespace in list still matches",
			specWith(map[string]string{
				ctrAnnotations.ContainerName: "agent",
				HostSidecarContainersKey:     " proxy , agent ",
			}),
			true,
		},
		{
			"crio container name matches",
			specWith(map[string]string{
				crioContainerNameKey:     "proxy",
				HostSidecarContainersKey: "proxy",
			}),
			true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.want, IsHostSidecar(tt.spec))
		})
	}
}
