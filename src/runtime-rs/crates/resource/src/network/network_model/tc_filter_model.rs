// Copyright (c) 2019-2022 Alibaba Cloud
// Copyright (c) 2019-2022 Ant Group
//
// SPDX-License-Identifier: Apache-2.0
//

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use futures::TryStreamExt;
use netlink_packet_route::tc::{TcAttribute, TcHandle};
use rtnetlink::Handle;
use scopeguard::defer;

use super::{NetworkModel, NetworkModelType};
use crate::network::NetworkPair;

const QDISC_ADD_ATTEMPTS: u64 = 5; // Number of attempts when adding an ingress qdisc
const QDISC_ADD_BACKOFF_MS: u64 = 10; // Base delay for the linear backoff between qdisc add retries on EBUSY
const INGRESS_PARENT: u32 = 0xffff0000;
const CLSACT_INGRESS_PARENT: u32 = 0xfffffff2;

#[derive(Debug)]
pub(crate) struct TcFilterModel {}

impl TcFilterModel {
    pub fn new() -> Result<Self> {
        Ok(Self {})
    }
}

#[async_trait]
impl NetworkModel for TcFilterModel {
    fn model_type(&self) -> NetworkModelType {
        NetworkModelType::TcFilter
    }

    async fn add(&self, pair: &NetworkPair) -> Result<()> {
        let (connection, handle, _) = rtnetlink::new_connection().context("new connection")?;
        let thread_handler = tokio::spawn(connection);

        defer!({
            thread_handler.abort();
        });

        let tap_index = fetch_index(&handle, pair.tap.tap_iface.name.as_str())
            .await
            .context("fetch tap by index")?;
        let virt_index = fetch_index(&handle, pair.virt_iface.name.as_str())
            .await
            .context("fetch virt by index")?;

        let tap_parent = add_ingress_qdisc(&handle, tap_index as i32)
            .await
            .context("add tap ingress")?;

        let virt_parent = add_ingress_qdisc(&handle, virt_index as i32)
            .await
            .context("add virt ingress")?;

        handle
            .traffic_filter(tap_index as i32)
            .add()
            .parent(tap_parent)
            // get protocol with network byte order
            .protocol(0x0003_u16.to_be())
            .redirect(virt_index)?
            .execute()
            .await
            .context("add redirect for tap")?;

        handle
            .traffic_filter(virt_index as i32)
            .add()
            .parent(virt_parent)
            // get protocol with network byte order
            .protocol(0x0003_u16.to_be())
            .redirect(tap_index)?
            .execute()
            .await
            .context("add redirect for virt")?;

        Ok(())
    }

    async fn del(&self, pair: &NetworkPair) -> Result<()> {
        let (connection, handle, _) = rtnetlink::new_connection().context("new connection")?;
        let thread_handler = tokio::spawn(connection);
        defer!({
            thread_handler.abort();
        });
        let virt_index = fetch_index(&handle, &pair.virt_iface.name).await?;
        handle.qdisc().del(virt_index as i32).execute().await?;
        Ok(())
    }
}

/// Add an ingress qdisc to the device at the given index, retrying on EBUSY
/// with linear backoff (10ms, 20ms, ...), and return the parent to attach
/// filters to. An ingress or clsact qdisc that another agent installed first
/// (e.g. the Datadog agent's network monitoring) is reused.
async fn add_ingress_qdisc(handle: &Handle, index: i32) -> Result<u32> {
    let mut last_err = handle.qdisc().add(index).ingress().execute().await;
    for i in 1..QDISC_ADD_ATTEMPTS {
        match &last_err {
            Ok(()) => break,
            Err(e) if !is_errno(e, libc::EBUSY) => break,
            Err(_) => {}
        }
        tokio::time::sleep(Duration::from_millis(QDISC_ADD_BACKOFF_MS * i)).await;
        last_err = handle.qdisc().add(index).ingress().execute().await;
    }
    match last_err {
        Ok(()) => Ok(INGRESS_PARENT),
        Err(e) if is_errno(&e, libc::EEXIST) => existing_ingress_parent(handle, index).await,
        Err(e) => Err(e.into()),
    }
}

async fn existing_ingress_parent(handle: &Handle, index: i32) -> Result<u32> {
    let mut qdiscs = handle.qdisc().get().index(index).execute();
    while let Some(qdisc) = qdiscs.try_next().await? {
        if qdisc.header.handle != TcHandle::from(INGRESS_PARENT) {
            continue;
        }
        for attr in &qdisc.attributes {
            match attr {
                TcAttribute::Kind(kind) if kind == "ingress" => return Ok(INGRESS_PARENT),
                TcAttribute::Kind(kind) if kind == "clsact" => return Ok(CLSACT_INGRESS_PARENT),
                _ => {}
            }
        }
    }
    Err(anyhow!(
        "device {index} has an ingress qdisc that is neither ingress nor clsact"
    ))
}

fn is_errno(err: &rtnetlink::Error, errno: i32) -> bool {
    match err {
        rtnetlink::Error::NetlinkError(msg) => msg.code.is_some_and(|c| c.get() == -errno),
        _ => false,
    }
}

pub async fn fetch_index(handle: &Handle, name: &str) -> Result<u32> {
    let link = crate::network::network_pair::get_link_by_name(handle, name)
        .await
        .context("get link by name")?;
    let base = link.attrs();
    Ok(base.index)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use futures::StreamExt;
    use kata_sys_util::netns::NetnsGuard;
    use netlink_packet_core::{
        NetlinkMessage, NetlinkPayload, NLM_F_ACK, NLM_F_CREATE, NLM_F_EXCL, NLM_F_REQUEST,
    };
    use netlink_packet_route::{tc::TcMessage, RouteNetlinkMessage};
    use netns_rs::NetNs;
    use rtnetlink::LinkDummy;
    use test_utils::skip_if_not_root;

    use super::*;
    use crate::network::network_pair::{NetworkInterface, TapInterface};

    async fn add_clsact(handle: &Handle, index: i32) -> Result<()> {
        let mut qdisc = TcMessage::with_index(index);
        qdisc.header.parent = TcHandle::INGRESS;
        qdisc.header.handle = TcHandle::from(INGRESS_PARENT);
        qdisc
            .attributes
            .push(TcAttribute::Kind("clsact".to_string()));
        let mut req = NetlinkMessage::from(RouteNetlinkMessage::NewQueueDiscipline(qdisc));
        req.header.flags = NLM_F_REQUEST | NLM_F_ACK | NLM_F_CREATE | NLM_F_EXCL;
        let mut response = handle.clone().request(req)?;
        while let Some(msg) = response.next().await {
            if let NetlinkPayload::Error(e) = msg.payload {
                if e.code.is_some() {
                    return Err(anyhow!("add clsact: {e:?}"));
                }
            }
        }
        Ok(())
    }

    #[test]
    fn test_add_reuses_existing_clsact() {
        skip_if_not_root!();

        let netns = NetNs::new("kata-tcfilter-test").unwrap();
        let path = netns.path().display().to_string();
        let result = std::thread::spawn(move || -> Result<()> {
            let _netns_guard = NetnsGuard::new(&path)?;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            rt.block_on(async {
                let (connection, handle, _) = rtnetlink::new_connection()?;
                tokio::spawn(connection);
                for name in ["eth0", "tap0_kata"] {
                    handle
                        .link()
                        .add(LinkDummy::new(name).build())
                        .execute()
                        .await?;
                }
                add_clsact(&handle, fetch_index(&handle, "eth0").await? as i32).await?;
                let pair = NetworkPair {
                    tap: TapInterface {
                        id: "test".to_owned(),
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
                    model: Arc::new(TcFilterModel::new()?),
                    network_qos: false,
                    network_queues: 1,
                };
                pair.add_network_model().await
            })
        })
        .join()
        .unwrap();
        netns.remove().unwrap();
        result.unwrap();
    }
}
