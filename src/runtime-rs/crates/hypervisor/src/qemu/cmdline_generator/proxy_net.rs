// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

use std::fs::File;
use std::os::fd::{AsRawFd, OwnedFd};

use anyhow::{Context, Result};
use async_trait::async_trait;
use kata_sys_util::netns::NetnsGuard;

use super::{
    bus_type, get_devno_ccw, should_disable_modern, QemuCmdLine, ToQemuParams, VirtioBusType,
};
use crate::utils::clear_cloexec;
use crate::{Address, NetworkBackend, NetworkConfig};

#[derive(Debug)]
struct SocketNetDevice {
    id: String,
    fd: File,
    mac_address: Address,
    bus_type: VirtioBusType,
    devno: Option<String>,
    disable_modern: bool,
    iommu_platform: bool,
}

impl SocketNetDevice {
    fn new(id: &str, fd: &OwnedFd, mac_address: Address, devno: Option<String>) -> Result<Self> {
        let fd = File::from(fd.try_clone().context("dup socket fd")?);
        clear_cloexec(fd.as_raw_fd()).context("clearing O_CLOEXEC failed")?;
        Ok(Self {
            id: id.to_owned(),
            fd,
            mac_address,
            bus_type: bus_type(),
            devno,
            disable_modern: should_disable_modern(),
            iommu_platform: false,
        })
    }
}

#[async_trait]
impl ToQemuParams for SocketNetDevice {
    async fn qemu_params(&self) -> Result<Vec<String>> {
        let mut device = vec![
            format!("virtio-net-{}", self.bus_type),
            format!("netdev={}", self.id),
            format!("mac={:?}", self.mac_address),
        ];
        if self.disable_modern && self.bus_type.supports_disable_modern() {
            device.push("disable-modern=true".to_owned());
        }
        if self.iommu_platform {
            device.push("iommu_platform=on".to_owned());
        }
        if let Some(devno) = &self.devno {
            device.push(format!("devno={devno}"));
        }

        Ok(vec![
            "-netdev".to_owned(),
            format!("socket,id={},fd={}", self.id, self.fd.as_raw_fd()),
            "-device".to_owned(),
            device.join(","),
        ])
    }
}

impl QemuCmdLine<'_> {
    pub fn add_network_device_with_backend(&mut self, config: &NetworkConfig) -> Result<()> {
        let guest_mac = config.guest_mac.clone().unwrap_or_default();
        let num_queues = config.queue_num.max(1) as u32;
        match &config.backend {
            NetworkBackend::Tap => {
                self.add_network_device(&config.host_dev_name, guest_mac, num_queues)
            }
            NetworkBackend::TapInNetns(netns) => {
                let _netns_guard = NetnsGuard::new(netns).context("new netns guard")?;
                self.add_network_device(&config.host_dev_name, guest_mac, num_queues)
            }
            NetworkBackend::SocketFd(fd) => {
                let id = format!("network-{}", config.host_dev_name);
                let devno = get_devno_ccw(&mut self.ccw_subchannel, &id);
                let mut device = SocketNetDevice::new(&id, fd, guest_mac, devno)?;
                device.iommu_platform = self.config.device_info.enable_iommu_platform
                    && device.bus_type == VirtioBusType::Ccw;
                self.devices.push(Box::new(device));
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    #[actix_rt::test]
    async fn test_socket_net_device_params() {
        let (vm, _peer) = UnixStream::pair().unwrap();
        let fd = OwnedFd::from(vm);
        let mut device =
            SocketNetDevice::new("network-tap0_kata", &fd, Address([2, 0, 0, 0, 0, 1]), None)
                .unwrap();
        device.bus_type = VirtioBusType::Pci;
        device.disable_modern = false;

        assert_ne!(device.fd.as_raw_fd(), fd.as_raw_fd());
        assert_eq!(
            device.qemu_params().await.unwrap(),
            vec![
                "-netdev".to_owned(),
                format!("socket,id=network-tap0_kata,fd={}", device.fd.as_raw_fd()),
                "-device".to_owned(),
                "virtio-net-pci,netdev=network-tap0_kata,mac=02:00:00:00:00:01".to_owned(),
            ]
        );
    }
}
