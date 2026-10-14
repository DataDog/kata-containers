// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

use anyhow::{anyhow, Context, Result};
use kata_sys_util::netns::NetnsGuard;

use super::Qmp;
use crate::device::DeviceType;
use crate::qemu::cmdline_generator::get_network_device;
use crate::utils::uses_native_ccw_bus;
use crate::{HypervisorConfig, NetworkBackend, NetworkDevice};

impl Qmp {
    pub fn hotplug_network_device_with_backend(
        &mut self,
        config: &HypervisorConfig,
        mut device: NetworkDevice,
    ) -> Result<DeviceType> {
        let net = &device.config;
        let guest_mac = net.guest_mac.clone().unwrap_or_default();
        let netdev_id = format!("network-{}", net.host_dev_name);
        match &net.backend {
            NetworkBackend::Tap => return Err(anyhow!("plain tap backend is not handled here")),
            NetworkBackend::TapInNetns(netns) => {
                let (netdev, virtio_net_device) = {
                    let _netns_guard = NetnsGuard::new(netns).context("new netns guard")?;
                    get_network_device(
                        config,
                        &net.host_dev_name,
                        guest_mac,
                        &mut None,
                        net.queue_num.max(1) as u32,
                    )?
                };
                self.hotplug_network_device(&netdev, &virtio_net_device)?;
            }
        }

        if !uses_native_ccw_bus() {
            let pci_path = self
                .get_device_by_qdev_id(&format!("frontend-{netdev_id}"))
                .context("get network device pci path")?;
            device.config.pci_path = Some(pci_path);
        }
        Ok(DeviceType::Network(device))
    }
}
