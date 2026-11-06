// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

//! Rootfs described by an `image_raw_block` Kata virtual volume: the container
//! image is a read-only block device (e.g. exported by the nydus snapshotter)
//! that is hot-plugged, mounted in the guest, and used as the lower of the
//! container rootfs overlay. Nothing is shared from the host.

use std::collections::HashMap;

use agent::Storage;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use base64::Engine;
use hypervisor::{
    device::{
        device_manager::{do_handle_device, get_block_device_info, DeviceManager},
        DeviceConfig, DeviceType,
    },
    BlockConfigModern, BlockDeviceAio,
};
use kata_types::{
    annotations::KATA_ANNO_VOLUME_ROOTFS_UPPER,
    mount::{
        KataVirtualVolume, Mount, KATA_VIRTUAL_VOLUME_IMAGE_RAW_BLOCK, KATA_VIRTUAL_VOLUME_PREFIX,
    },
};
use oci_spec::runtime as oci;
use tokio::sync::RwLock;

use super::{erofs_rootfs::DEFAULT_KATA_GUEST_ROOT_SHARED_FS, rootfs_upper, Rootfs};
use crate::block_device::agent_storage_source_from_block_config;

pub(crate) struct RawBlockRootfs {
    guest_path: String,
    device_id: String,
    storages: Vec<Storage>,
}

/// Returns the `image_raw_block` virtual volume of a rootfs mount, if any.
pub(crate) fn raw_block_volume(m: &Mount) -> Result<Option<KataVirtualVolume>> {
    for o in &m.options {
        if let Some(encoded) = o.strip_prefix(KATA_VIRTUAL_VOLUME_PREFIX) {
            let volume =
                KataVirtualVolume::from_base64(encoded).context("decode kata virtual volume")?;
            if volume.volume_type == KATA_VIRTUAL_VOLUME_IMAGE_RAW_BLOCK {
                volume
                    .validate()
                    .context("invalid raw block virtual volume")?;
                return Ok(Some(volume));
            }
        }
    }
    Ok(None)
}

impl RawBlockRootfs {
    pub async fn new(
        d: &RwLock<DeviceManager>,
        cid: &str,
        volume: &KataVirtualVolume,
        annotations: &HashMap<String, String>,
    ) -> Result<Self> {
        let blkdev_info = get_block_device_info(d).await;
        let config = BlockConfigModern {
            driver_option: blkdev_info.block_device_driver.clone(),
            path_on_host: volume.source.clone(),
            // The image is read-only: containers only write to the overlay upper.
            is_readonly: true,
            blkdev_aio: BlockDeviceAio::new(&blkdev_info.block_device_aio),
            num_queues: blkdev_info.num_queues,
            queue_size: blkdev_info.queue_size,
            ..Default::default()
        };
        let device_info = do_handle_device(d, &DeviceConfig::BlockCfgModern(config))
            .await
            .context("attach raw block rootfs device")?;
        let DeviceType::BlockModern(device) = device_info else {
            return Err(anyhow!("unexpected device for raw block rootfs"));
        };
        let device = device.lock().await.clone();

        let image_mount = image_mount_point(&volume.source);
        let image = Storage {
            driver: device.config.driver_option.clone(),
            source: agent_storage_source_from_block_config(&device.config)?,
            fs_type: volume.fs_type.clone(),
            options: volume.options.clone(),
            mount_point: image_mount.clone(),
            ..Default::default()
        };
        // Upper/work go on the rootfs-upper block volume when the pod requests one, else in the
        // guest.
        let upper = annotations
            .get(KATA_ANNO_VOLUME_ROOTFS_UPPER)
            .filter(|u| !u.is_empty())
            .cloned()
            .unwrap_or_else(|| format!("{DEFAULT_KATA_GUEST_ROOT_SHARED_FS}{cid}"));
        let rootfs = rootfs_upper::overlay_storage(cid, &image_mount, &upper);

        Ok(Self {
            guest_path: rootfs.mount_point.clone(),
            device_id: device.device_id,
            storages: vec![image, rootfs],
        })
    }
}

/// Where the guest mounts the image. Keyed by the source so that containers of
/// the same image in a sandbox share the mount.
fn image_mount_point(source: &str) -> String {
    let name = base64::engine::general_purpose::URL_SAFE.encode(source);
    format!("{DEFAULT_KATA_GUEST_ROOT_SHARED_FS}virtual-volumes/{name}")
}

#[async_trait]
impl Rootfs for RawBlockRootfs {
    async fn get_guest_rootfs_path(&self) -> Result<String> {
        Ok(self.guest_path.clone())
    }

    async fn get_rootfs_mount(&self) -> Result<Vec<oci::Mount>> {
        Ok(vec![])
    }

    async fn get_storage(&self) -> Option<Vec<Storage>> {
        Some(self.storages.clone())
    }

    async fn get_device_id(&self) -> Result<Option<String>> {
        Ok(Some(self.device_id.clone()))
    }

    async fn cleanup(&self, device_manager: &RwLock<DeviceManager>) -> Result<()> {
        device_manager
            .write()
            .await
            .try_remove_device(&self.device_id)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mount_with(options: Vec<String>) -> Mount {
        Mount {
            fs_type: "fuse.nydus-overlayfs".to_string(),
            options,
            ..Default::default()
        }
    }

    fn volume_option(volume: &KataVirtualVolume) -> String {
        format!(
            "{KATA_VIRTUAL_VOLUME_PREFIX}{}",
            base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(volume).unwrap())
        )
    }

    #[test]
    fn test_raw_block_volume() {
        let raw_block = KataVirtualVolume {
            volume_type: KATA_VIRTUAL_VOLUME_IMAGE_RAW_BLOCK.to_string(),
            source: "/dev/nbd0".to_string(),
            fs_type: "erofs".to_string(),
            options: vec!["ro".to_string()],
            ..Default::default()
        };
        let guest_pull = KataVirtualVolume {
            volume_type: kata_types::mount::KATA_VIRTUAL_VOLUME_IMAGE_GUEST_PULL.to_string(),
            ..Default::default()
        };
        let no_fs_type = KataVirtualVolume {
            fs_type: String::new(),
            ..raw_block.clone()
        };

        let cases = [
            ("no volume", vec!["lowerdir=/a".to_string()], Ok(None)),
            (
                "raw block",
                vec!["lowerdir=/a".to_string(), volume_option(&raw_block)],
                Ok(Some(raw_block.clone())),
            ),
            ("guest pull", vec![volume_option(&guest_pull)], Ok(None)),
            (
                "invalid raw block",
                vec![volume_option(&no_fs_type)],
                Err(()),
            ),
            (
                "malformed",
                vec![format!("{KATA_VIRTUAL_VOLUME_PREFIX}!!")],
                Err(()),
            ),
        ];
        for (name, options, want) in cases {
            let got = raw_block_volume(&mount_with(options)).map_err(|_| ());
            assert_eq!(got, want, "{name}");
        }
    }
}
