// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

use std::net::Ipv4Addr;
use std::sync::Arc;

use agent::{ARPNeighbor, IPAddress, IPFamily, Interface, Route};
use anyhow::{Context, Result};
use async_trait::async_trait;
use hypervisor::device::device_manager::DeviceManager;
use tokio::sync::RwLock;

use super::endpoint::{Endpoint, VethEndpoint};
use super::network_info::network_info_from_link::NetworkInfoFromLink;
use super::network_model::{self, JAILNET_NET_MODEL_STR, TAPNET_NET_MODEL_STR};
use super::network_pair::{NetworkInterface, NetworkPair, TapInterface};
use super::network_with_netns::NetworkWithNetNsConfig;
use super::utils::{self, link};
use super::NetworkInfo;

pub(crate) const PROXY_TAP_NET: Ipv4Addr = Ipv4Addr::new(169, 254, 1, 0);
pub(crate) const PROXY_TAP_HOST_IP: Ipv4Addr = Ipv4Addr::new(169, 254, 1, 1);
const PROXY_TAP_VM_IP: Ipv4Addr = Ipv4Addr::new(169, 254, 1, 2);
pub(crate) const PROXY_PREFIX_LEN: u8 = 30;

pub(crate) fn is_host_side_proxy_model(model: &str) -> bool {
    model == JAILNET_NET_MODEL_STR || model == TAPNET_NET_MODEL_STR
}

// No tap is created in the pod netns: the network model provides the VM backend.
// The ID is derived from the sandbox ID so the proxy can compute the netns/socket names.
pub(crate) async fn create_endpoint(
    handle: &rtnetlink::Handle,
    link: &dyn link::Link,
    addrs: Vec<IPAddress>,
    idx: u32,
    config: &NetworkWithNetNsConfig,
    d: Arc<RwLock<DeviceManager>>,
) -> Result<(Arc<dyn Endpoint>, Arc<dyn NetworkInfo>)> {
    let hw_addr = utils::get_mac_addr(&link.attrs().hardware_addr).context("get mac addr")?;
    let net_pair = NetworkPair {
        tap: TapInterface {
            id: format!("{}-{idx}", config.sandbox_id),
            name: format!("br{idx}_kata"),
            tap_iface: NetworkInterface {
                name: format!("tap{idx}_kata"),
                hard_addr: hw_addr.clone(),
                ..Default::default()
            },
        },
        virt_iface: NetworkInterface {
            name: link.attrs().name.clone(),
            ..Default::default()
        },
        model: network_model::new(&config.network_model).context("new network model")?,
        network_qos: false,
        network_queues: config.queues.max(1),
    };
    let network_info = NetworkInfoFromLink::new(handle, link, addrs, &hw_addr)
        .await
        .context("network info from link")?;

    Ok((
        Arc::new(VethEndpoint { net_pair, d }),
        Arc::new(HostSideProxyNetworkInfo(network_info)),
    ))
}

#[derive(Debug)]
struct HostSideProxyNetworkInfo(NetworkInfoFromLink);

#[async_trait]
impl NetworkInfo for HostSideProxyNetworkInfo {
    async fn interface(&self) -> Result<Interface> {
        let mut iface = self.0.interface().await?;
        iface.ip_addresses = vec![IPAddress {
            family: IPFamily::V4,
            address: PROXY_TAP_VM_IP.to_string(),
            mask: PROXY_PREFIX_LEN.to_string(),
        }];
        Ok(iface)
    }

    async fn routes(&self) -> Result<Vec<Route>> {
        let device = self.0.interface().await?.name;
        Ok(vec![
            Route {
                dest: format!("{PROXY_TAP_NET}/{PROXY_PREFIX_LEN}"),
                device: device.clone(),
                scope: libc::RT_SCOPE_LINK as u32,
                ..Default::default()
            },
            Route {
                gateway: PROXY_TAP_HOST_IP.to_string(),
                device,
                ..Default::default()
            },
        ])
    }

    async fn neighs(&self) -> Result<Vec<ARPNeighbor>> {
        self.0.neighs().await
    }

    async fn set_device_path(&self, path: String) -> Result<()> {
        self.0.set_device_path(path).await
    }
}
