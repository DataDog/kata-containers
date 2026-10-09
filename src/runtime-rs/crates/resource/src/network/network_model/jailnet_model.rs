// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

use std::fs;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::thread;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use hypervisor::NetworkBackend;
use kata_sys_util::netns::NetnsGuard;
use netlink_packet_route::route::RouteProtocol;
use netns_rs::{Env, NetNs};
use nix::mount::{umount2, MntFlags};
use rtnetlink::{Handle, LinkUnspec, LinkVeth, RouteMessageBuilder};
use scopeguard::defer;

use super::{NetworkModel, NetworkModelType};
use crate::network::host_side_proxy::{PROXY_PREFIX_LEN, PROXY_TAP_HOST_IP, PROXY_TAP_NET};
use crate::network::network_pair::{create_link, get_link_by_name};
use crate::network::NetworkPair;

pub(crate) const POD_VETH_NAME: &str = "kata-pvp";
const JAIL_VETH_NAME: &str = "kata-pvj";
const JAIL_VETH_IP: Ipv4Addr = Ipv4Addr::new(169, 254, 2, 1);
const POD_VETH_IP: Ipv4Addr = Ipv4Addr::new(169, 254, 2, 2);
const NETNS_DIR: &str = "/run/netns";

struct JailNetnsEnv;

impl Env for JailNetnsEnv {
    fn persist_dir(&self) -> PathBuf {
        PathBuf::from(NETNS_DIR)
    }
}

fn jail_netns_name(id: &str) -> String {
    format!("kata-jail-{id}")
}

fn jail_netns_path(id: &str) -> String {
    format!("{NETNS_DIR}/{}", jail_netns_name(id))
}

#[derive(Debug)]
pub(crate) struct JailNetModel {}

impl JailNetModel {
    pub fn new() -> Result<Self> {
        Ok(Self {})
    }
}

#[async_trait]
impl NetworkModel for JailNetModel {
    fn model_type(&self) -> NetworkModelType {
        NetworkModelType::JailNet
    }

    fn backend(&self, pair: &NetworkPair) -> NetworkBackend {
        NetworkBackend::TapInNetns(jail_netns_path(&pair.tap.id))
    }

    async fn add(&self, pair: &NetworkPair) -> Result<()> {
        let netns_path = jail_netns_path(&pair.tap.id);
        let (connection, handle, _) = rtnetlink::new_connection().context("new connection")?;
        let thread_handler = tokio::spawn(connection);
        defer!({
            thread_handler.abort();
        });

        if remove_netns(&netns_path).is_err() {
            let _ = fs::remove_file(&netns_path);
        }
        if let Ok(link) = get_link_by_name(&handle, POD_VETH_NAME).await {
            let _ = handle.link().del(link.attrs().index).execute().await;
        }

        let mtu = get_link_by_name(&handle, &pair.virt_iface.name)
            .await
            .context("get virt link")?
            .attrs()
            .mtu;
        let jail = NetNs::new_with_env(jail_netns_name(&pair.tap.id), JailNetnsEnv)
            .context("create jail netns")?;

        handle
            .link()
            .add(LinkVeth::new(POD_VETH_NAME, JAIL_VETH_NAME).build())
            .execute()
            .await
            .context("create veth pair")?;
        let jail_veth = get_link_by_name(&handle, JAIL_VETH_NAME).await?;
        handle
            .link()
            .set(
                LinkUnspec::new_with_index(jail_veth.attrs().index)
                    .setns_by_fd(jail.file().as_raw_fd())
                    .build(),
            )
            .execute()
            .await
            .context("move veth into jail netns")?;

        let tap_name = pair.tap.tap_iface.name.clone();
        let queues = pair.network_queues;
        run_in_netns(netns_path, move |handle| async move {
            create_link(&handle, &tap_name, queues)
                .await
                .context("create tap")?;
            configure_link(&handle, &tap_name, PROXY_TAP_HOST_IP, mtu).await?;
            let veth = configure_link(&handle, JAIL_VETH_NAME, JAIL_VETH_IP, mtu).await?;
            handle
                .route()
                .add(
                    RouteMessageBuilder::<Ipv4Addr>::new()
                        .protocol(RouteProtocol::Boot)
                        .output_interface(veth)
                        .gateway(POD_VETH_IP)
                        .build(),
                )
                .execute()
                .await
                .context("add jail default route")?;
            fs::write("/proc/sys/net/ipv4/ip_forward", "1").context("enable ip_forward")
        })?;

        let veth = configure_link(&handle, POD_VETH_NAME, POD_VETH_IP, mtu).await?;
        handle
            .route()
            .add(
                RouteMessageBuilder::<Ipv4Addr>::new()
                    .protocol(RouteProtocol::Boot)
                    .destination_prefix(PROXY_TAP_NET, PROXY_PREFIX_LEN)
                    .output_interface(veth)
                    .gateway(JAIL_VETH_IP)
                    .build(),
            )
            .execute()
            .await
            .context("add pod route to VM subnet")
    }

    async fn del(&self, pair: &NetworkPair) -> Result<()> {
        let (connection, handle, _) = rtnetlink::new_connection().context("new connection")?;
        let thread_handler = tokio::spawn(connection);
        defer!({
            thread_handler.abort();
        });

        if let Ok(link) = get_link_by_name(&handle, POD_VETH_NAME).await {
            if let Err(e) = handle.link().del(link.attrs().index).execute().await {
                warn!(sl!(), "jailnet: failed to delete {}: {}", POD_VETH_NAME, e);
            }
        }

        let netns_path = jail_netns_path(&pair.tap.id);
        let tap_name = pair.tap.tap_iface.name.clone();
        run_in_netns(netns_path.clone(), move |handle| async move {
            if let Ok(tap) = get_link_by_name(&handle, &tap_name).await {
                if let Err(e) = handle.link().del(tap.attrs().index).execute().await {
                    warn!(sl!(), "jailnet: failed to delete {}: {}", tap_name, e);
                }
            }
            Ok(())
        })?;
        remove_netns(&netns_path)
    }
}

async fn configure_link(handle: &Handle, name: &str, ip: Ipv4Addr, mtu: u32) -> Result<u32> {
    let index = get_link_by_name(handle, name).await?.attrs().index;
    handle
        .address()
        .add(index, IpAddr::V4(ip), PROXY_PREFIX_LEN)
        .execute()
        .await
        .with_context(|| format!("add address to {name}"))?;
    handle
        .link()
        .set(LinkUnspec::new_with_index(index).mtu(mtu).up().build())
        .execute()
        .await
        .with_context(|| format!("set {name} up"))?;
    Ok(index)
}

fn remove_netns(path: &str) -> Result<()> {
    umount2(path, MntFlags::MNT_DETACH).with_context(|| format!("umount {path}"))?;
    fs::remove_file(path).with_context(|| format!("remove {path}"))
}

// Runs `f` with a netlink handle bound to `netns_path`, on a dedicated thread
// so the netns switch cannot leak into other tasks of the caller's runtime.
fn run_in_netns<F, Fut>(netns_path: String, f: F) -> Result<()>
where
    F: FnOnce(Handle) -> Fut + Send + 'static,
    Fut: Future<Output = Result<()>>,
{
    thread::spawn(move || -> Result<()> {
        let _netns_guard = NetnsGuard::new(&netns_path).context("enter jail netns")?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()?;
        rt.block_on(async move {
            let (connection, handle, _) = rtnetlink::new_connection().context("new connection")?;
            let thread_handler = tokio::spawn(connection);
            defer!({
                thread_handler.abort();
            });
            f(handle).await
        })
    })
    .join()
    .map_err(|e| anyhow!("{:?}", e))?
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use anyhow::ensure;
    use rtnetlink::LinkDummy;
    use test_utils::skip_if_not_root;

    use super::*;
    use crate::network::network_pair::{NetworkInterface, TapInterface};

    async fn ensure_mtu(handle: &Handle, name: &str, mtu: u32) -> Result<()> {
        let got = get_link_by_name(handle, name).await?.attrs().mtu;
        ensure!(got == mtu, "{} MTU = {}, want {}", name, got, mtu);
        Ok(())
    }

    #[test]
    fn test_jailnet_preserves_mtu() {
        skip_if_not_root!();

        for mtu in [1280, 1500, 9001] {
            let pod = NetNs::new(format!("kata-jailnet-test-{}", mtu)).unwrap();
            let result = run_in_netns(pod.path().display().to_string(), move |handle| async move {
                handle
                    .link()
                    .add(LinkDummy::new("eth0").mtu(mtu).build())
                    .execute()
                    .await?;
                let pair = NetworkPair {
                    tap: TapInterface {
                        id: format!("test-{}", mtu),
                        name: "br0_kata".to_owned(),
                        tap_iface: NetworkInterface {
                            name: "tap0_kata".to_owned(),
                            ..Default::default()
                        },
                    },
                    virt_iface: NetworkInterface {
                        name: "eth0".to_owned(),
                        ..Default::default()
                    },
                    model: Arc::new(JailNetModel::new()?),
                    network_qos: false,
                    network_queues: 1,
                };

                pair.add_network_model().await?;
                let mut checked = ensure_mtu(&handle, POD_VETH_NAME, mtu).await;
                if checked.is_ok() {
                    checked = run_in_netns(jail_netns_path(&pair.tap.id), move |jail| async move {
                        ensure_mtu(&jail, JAIL_VETH_NAME, mtu).await?;
                        ensure_mtu(&jail, "tap0_kata", mtu).await
                    });
                }
                pair.del_network_model().await?;
                checked
            });
            pod.remove().unwrap();
            result.unwrap();
        }
    }
}
