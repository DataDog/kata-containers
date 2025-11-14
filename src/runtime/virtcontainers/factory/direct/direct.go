// Copyright (c) 2018 HyperHQ Inc.
//
// SPDX-License-Identifier: Apache-2.0
//
// direct implements base vm factory without vm templating.

package direct

import (
	"context"

	pb "github.com/kata-containers/kata-containers/src/runtime/protocols/cache"
	vc "github.com/kata-containers/kata-containers/src/runtime/virtcontainers"
	"github.com/kata-containers/kata-containers/src/runtime/virtcontainers/factory/base"
)

type direct struct {
	config vc.VMConfig
}

// New returns a new direct vm factory.
func New(ctx context.Context, config vc.VMConfig) base.FactoryBase {
	return &direct{config}
}

// Config returns the direct factory's configuration.
func (d *direct) Config() vc.VMConfig {
	return d.config
}

// GetBaseVM create a new VM directly.
func (d *direct) GetBaseVM(ctx context.Context, config vc.VMConfig) (*vc.VM, error) {
	vm, err := vc.NewVM(ctx, config)
	if err != nil {
		return nil, err
	}

	// Don't pause VMs using VirtioFS - the vhost-user connection would be disrupted
	// For VirtioFS, we keep the VM running to maintain the virtiofsd daemon connection
	if config.HypervisorConfig.SharedFS == "virtio-fs" || config.HypervisorConfig.SharedFS == "virtio-fs-nydus" {
		// VM stays running - no pause needed for VirtioFS
		return vm, nil
	}

	// For other shared filesystem types (9p, etc), pause as usual
	err = vm.Pause(ctx)
	if err != nil {
		vm.Stop(ctx)
		return nil, err
	}

	return vm, nil
}

// CloseFactory closes the direct vm factory.
func (d *direct) CloseFactory(ctx context.Context) {
}

// GetVMStatus is not supported
func (d *direct) GetVMStatus() []*pb.GrpcVMStatus {
	panic("ERROR: package direct does not support GetVMStatus")
}
