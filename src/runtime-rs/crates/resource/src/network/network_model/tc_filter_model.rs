// Copyright (c) 2019-2022 Alibaba Cloud
// Copyright (c) 2019-2022 Ant Group
//
// SPDX-License-Identifier: Apache-2.0
//

use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use rtnetlink::Handle;
use scopeguard::defer;

use super::{NetworkModel, NetworkModelType};
use crate::network::NetworkPair;

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

        add_ingress_qdisc(&handle, tap_index as i32)
            .await
            .context("add tap ingress")?;

        add_ingress_qdisc(&handle, virt_index as i32)
            .await
            .context("add virt ingress")?;

        handle
            .traffic_filter(tap_index as i32)
            .add()
            .parent(0xffff0000)
            // get protocol with network byte order
            .protocol(0x0003_u16.to_be())
            .redirect(virt_index)?
            .execute()
            .await
            .context("add redirect for tap")?;

        handle
            .traffic_filter(virt_index as i32)
            .add()
            .parent(0xffff0000)
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

/// Add an ingress qdisc to the device at the given index, retrying up to 5
/// times on EBUSY with linear backoff (10ms, 20ms, …, 50ms).
async fn add_ingress_qdisc(handle: &Handle, index: i32) -> Result<(), rtnetlink::Error> {
    let mut last_err = None;
    for i in 0u64..5 {
        match handle.qdisc().add(index).ingress().execute().await {
            Ok(()) => return Ok(()),
            Err(e) => {
                if !is_ebusy(&e) {
                    return Err(e);
                }
                last_err = Some(e);
                tokio::time::sleep(Duration::from_millis(10 * (i + 1))).await;
            }
        }
    }
    Err(last_err.unwrap())
}

fn is_ebusy(err: &rtnetlink::Error) -> bool {
    match err {
        rtnetlink::Error::NetlinkError(msg) => {
            msg.code
                .map_or(false, |c| c.get() == -(libc::EBUSY as i32))
        }
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
