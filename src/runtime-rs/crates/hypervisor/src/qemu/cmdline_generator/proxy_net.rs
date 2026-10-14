// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

use anyhow::{Context, Result};
use kata_sys_util::netns::NetnsGuard;

use super::QemuCmdLine;
use crate::{NetworkBackend, NetworkConfig};

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
        }
    }
}
