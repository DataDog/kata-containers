// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

use std::os::fd::{AsRawFd, OwnedFd};

use anyhow::{anyhow, Context, Result};
use kata_sys_util::netns::NetnsGuard;
use qapi_qmp as qmp;
use qapi_spec::Dictionary;

use super::Qmp;
use crate::device::DeviceType;
use crate::qemu::cmdline_generator::get_network_device;
use crate::utils::uses_native_ccw_bus;
use crate::{Address, HypervisorConfig, NetworkBackend, NetworkDevice};

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
            NetworkBackend::SocketFd(fd) => {
                self.hotplug_socket_network_device(&netdev_id, fd, guest_mac)?
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

    fn hotplug_socket_network_device(
        &mut self,
        netdev_id: &str,
        fd: &OwnedFd,
        guest_mac: Address,
    ) -> Result<()> {
        let frontend_id = format!("frontend-{netdev_id}");
        let mut args = Dictionary::new();
        args.insert("netdev".to_owned(), netdev_id.into());
        args.insert("mac".to_owned(), format!("{guest_mac:?}").into());

        let (driver, pci_slot) = if uses_native_ccw_bus() {
            let subchannel = self
                .ccw_subchannel
                .as_mut()
                .ok_or_else(|| anyhow!("CCW subchannel not available"))?;
            let slot = subchannel
                .add_device(&frontend_id)
                .map_err(|e| anyhow!("CCW subchannel add_device failed: {:?}", e))?;
            args.insert(
                "devno".to_owned(),
                subchannel.address_format_ccw(slot).into(),
            );
            ("virtio-net-ccw", None)
        } else {
            let (bus, slot) = self.find_free_slot()?;
            args.insert("addr".to_owned(), format!("{slot:02}").into());
            ("virtio-net-pci", Some((bus, slot)))
        };

        self.pass_fd(fd.as_raw_fd(), "fd0")?;
        self.qmp.execute(&qmp::netdev_add(qmp::Netdev::socket {
            id: netdev_id.to_owned(),
            socket: qmp::NetdevSocketOptions {
                fd: Some("fd0".to_owned()),
                listen: None,
                connect: None,
                mcast: None,
                localaddr: None,
                udp: None,
            },
        }))?;

        if let Err(e) = self.qmp.execute(&qmp::device_add {
            bus: pci_slot.as_ref().map(|(bus, _)| bus.clone()),
            id: Some(frontend_id.clone()),
            driver: driver.to_owned(),
            arguments: args,
        }) {
            let _ = self.qmp.execute(&qmp::netdev_del {
                id: netdev_id.to_owned(),
            });
            return Err(e.into());
        }

        if let Some((bus, slot)) = pci_slot {
            self.record_pci_bridge_slot(&bus, slot, &frontend_id);
        }
        Ok(())
    }
}
