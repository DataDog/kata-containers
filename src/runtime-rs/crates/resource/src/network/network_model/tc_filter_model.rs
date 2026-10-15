// Copyright (c) 2019-2022 Alibaba Cloud
// Copyright (c) 2019-2022 Ant Group
//
// SPDX-License-Identifier: Apache-2.0
//

use std::sync::atomic::{AtomicBool, Ordering};
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

/// One model per NetworkPair, so the flags below are this pair's history:
/// whether an attach of this pair added the ingress qdisc on its TAP and
/// virt device. A retried attach forgives EEXIST only for a qdisc it added
/// itself; one somebody else put on the device fails the attach loudly, as
/// a first attach always did.
#[derive(Debug, Default)]
pub(crate) struct TcFilterModel {
    tap_qdisc_added: AtomicBool,
    virt_qdisc_added: AtomicBool,
}

impl TcFilterModel {
    pub fn new() -> Result<Self> {
        Ok(Self::default())
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

        let tap_parent = add_ingress_qdisc(&handle, tap_index as i32, &self.tap_qdisc_added)
            .await
            .context("add tap ingress")?;

        let virt_parent = add_ingress_qdisc(&handle, virt_index as i32, &self.virt_qdisc_added)
            .await
            .context("add virt ingress")?;

        // A retried attach can add a redirect its failed predecessor already
        // added. The kernel appends it as another filter instance; the
        // earlier one steals every matching packet first, so the duplicate
        // changes nothing and goes away with the qdisc.
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
        let mut links = handle
            .link()
            .get()
            .match_name(pair.virt_iface.name.clone())
            .execute();
        let virt_index = match links.try_next().await {
            Ok(Some(link)) => link.header.index,
            Ok(None) => return Ok(()),
            Err(err) if is_absent(&err) => return Ok(()),
            Err(err) => return Err(err.into()),
        };
        match handle.qdisc().del(virt_index as i32).execute().await {
            Ok(()) => {}
            Err(err) if is_absent(&err) => {}
            Err(err) => return Err(err.into()),
        }
        Ok(())
    }
}

fn is_absent(err: &rtnetlink::Error) -> bool {
    matches!(
        err,
        rtnetlink::Error::NetlinkError(msg)
            if msg.code.is_some_and(|code| {
                code.get() == -libc::ENOENT || code.get() == -libc::ENODEV
            })
    )
}

/// Add an ingress qdisc to the device at the given index, retrying on EBUSY
/// with linear backoff (10ms, 20ms, ...), and return the parent to attach
/// filters to. An EEXIST for the qdisc an earlier attempt of this attach
/// added keeps it; an ingress or clsact qdisc that another agent installed
/// first (e.g. the Datadog agent's network monitoring) is reused.
async fn add_ingress_qdisc(handle: &Handle, index: i32, added_before: &AtomicBool) -> Result<u32> {
    let mut last_err = handle.qdisc().add(index).ingress().execute().await;
    for i in 1..QDISC_ADD_ATTEMPTS {
        match &last_err {
            Ok(()) => break,
            Err(e) if !is_ebusy(e) => break,
            Err(_) => {}
        }
        tokio::time::sleep(Duration::from_millis(QDISC_ADD_BACKOFF_MS * i)).await;
        last_err = handle.qdisc().add(index).ingress().execute().await;
    }
    match last_err {
        Ok(()) => {
            added_before.store(true, Ordering::SeqCst);
            Ok(INGRESS_PARENT)
        }
        Err(ref e) if is_eexist(e) && added_before.load(Ordering::SeqCst) => Ok(INGRESS_PARENT),
        Err(ref e) if is_eexist(e) => existing_ingress_parent(handle, index).await,
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

fn is_ebusy(err: &rtnetlink::Error) -> bool {
    match err {
        rtnetlink::Error::NetlinkError(msg) => msg.code.is_some_and(|c| c.get() == -libc::EBUSY),
        _ => false,
    }
}

fn is_eexist(err: &rtnetlink::Error) -> bool {
    match err {
        rtnetlink::Error::NetlinkError(msg) => msg.code.is_some_and(|c| c.get() == -libc::EEXIST),
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
    use crate::network::utils::link::{create_link, net_test_utils::delete_link, LinkType};

    // Runs in the sudo test pass; a retried attach after one that failed
    // between the TC setup and the device hotplug finds the ingress qdisc
    // it added before, and only that must not fail the retry: a qdisc
    // somebody else put on the device is not Kata's to adopt.
    #[tokio::test]
    async fn a_retried_qdisc_add_forgives_only_its_own_earlier_work() {
        skip_if_not_root!();
        let name = "___test_qdisc";
        let (connection, handle, _) = rtnetlink::new_connection().unwrap();
        let thread_handler = tokio::spawn(connection);
        defer!({
            thread_handler.abort();
        });

        create_link(name, LinkType::Tap, 1).unwrap();
        let index = fetch_index(&handle, name).await.unwrap() as i32;
        let added = AtomicBool::new(false);
        add_ingress_qdisc(&handle, index, &added).await.unwrap();
        assert!(added.load(Ordering::SeqCst));
        add_ingress_qdisc(&handle, index, &added).await.unwrap();

        // To a fresh attach the same qdisc is foreign, and it is reused.
        let foreign = AtomicBool::new(false);
        assert_eq!(
            add_ingress_qdisc(&handle, index, &foreign).await.unwrap(),
            INGRESS_PARENT
        );
        assert!(!foreign.load(Ordering::SeqCst));
        delete_link(&handle, name).await.unwrap();
    }

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

    #[tokio::test]
    async fn deleting_tc_filter_after_cni_removes_interface_is_idempotent() {
        let model = TcFilterModel::new().unwrap();
        let pair = NetworkPair {
            tap: TapInterface::default(),
            virt_iface: NetworkInterface {
                name: "kata_missing0".into(),
                ..Default::default()
            },
            model: Arc::new(TcFilterModel::new().unwrap()),
            network_qos: false,
            network_queues: 1,
        };

        model.del(&pair).await.unwrap();
        model.del(&pair).await.unwrap();
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
