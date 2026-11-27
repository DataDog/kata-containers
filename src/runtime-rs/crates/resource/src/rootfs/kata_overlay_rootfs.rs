// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

//! Rootfs assembled in the guest as an overlay of the layers described by
//! `io.katacontainers.fs-opt.layer` mount options ("kata overlays"): the
//! runtime-rs counterpart of the Go runtime's layer branch of
//! `ShareRootFilesystem`.
//!
//! The layers are attached and mounted in the guest at their destinations
//! under `KATA_GUEST_SANDBOX_LAYERS_DIR` (see [`super::layers`]); this rootfs
//! is a single overlay storage whose options come from the rootfs mount, with
//! the `io.katacontainers.*` transport options removed — the guest mount
//! would otherwise reject them.

use agent::Storage;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use hypervisor::device::device_manager::DeviceManager;
use kata_types::device::DRIVER_OVERLAYFS_TYPE;
use kata_types::mount::{Mount, KATA_FS_OPT_LAYER_PREFIX};
use oci_spec::runtime as oci;
use tokio::sync::RwLock;

use super::layers::filter_layer_storage_options;
use super::{erofs_rootfs::DEFAULT_KATA_GUEST_ROOT_SHARED_FS, Rootfs, ROOTFS, TYPE_OVERLAY_FS};

/// Whether a rootfs mount describes a kata overlay: it carries layer options
/// and is assembled in the guest, never mounted on the host.
pub(crate) fn has_layer_options(m: &Mount) -> bool {
    m.options
        .iter()
        .any(|o| o.starts_with(KATA_FS_OPT_LAYER_PREFIX))
}

pub(crate) struct KataOverlayRootfs {
    guest_path: String,
    storage: Storage,
}

impl KataOverlayRootfs {
    pub(crate) fn new(cid: &str, m: &Mount) -> Result<Self> {
        if m.fs_type.is_empty() {
            return Err(anyhow!("kata overlay rootfs mount has no filesystem type"));
        }
        let guest_path = format!("{DEFAULT_KATA_GUEST_ROOT_SHARED_FS}{cid}/{ROOTFS}");
        let storage = Storage {
            driver: DRIVER_OVERLAYFS_TYPE.to_string(),
            source: TYPE_OVERLAY_FS.to_string(),
            fs_type: m.fs_type.clone(),
            options: filter_layer_storage_options(&m.options),
            mount_point: guest_path.clone(),
            ..Default::default()
        };
        Ok(Self {
            guest_path,
            storage,
        })
    }
}

#[async_trait]
impl Rootfs for KataOverlayRootfs {
    async fn get_guest_rootfs_path(&self) -> Result<String> {
        Ok(self.guest_path.clone())
    }

    async fn get_rootfs_mount(&self) -> Result<Vec<oci::Mount>> {
        Ok(vec![])
    }

    async fn get_storage(&self) -> Option<Vec<Storage>> {
        Some(vec![self.storage.clone()])
    }

    async fn get_device_ids(&self) -> Result<Vec<String>> {
        // Layer devices belong to the layer volumes, cleaned up with them.
        Ok(Vec::new())
    }

    async fn cleanup(&self, _device_manager: &RwLock<DeviceManager>) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kata_types::mount::KATA_FS_OPT_IS_LAYER;

    #[test]
    fn test_has_layer_options() {
        let cases = [
            (
                "layer option",
                vec!["io.katacontainers.fs-opt.layer=/dev/nbd2,ext4,rw"],
                true,
            ),
            (
                "with volume option too",
                vec![
                    "io.katacontainers.volume.eyJh",
                    "io.katacontainers.fs-opt.layer=/a,erofs,ro",
                ],
                true,
            ),
            ("no layer option", vec!["rw", "lowerdir=/a"], false),
            ("no options", vec![], false),
        ];
        for (name, options, want) in cases {
            let m = Mount {
                fs_type: "overlay".to_string(),
                options: options.iter().map(|o| o.to_string()).collect(),
                ..Default::default()
            };
            assert_eq!(has_layer_options(&m), want, "{name}");
        }
    }

    #[test]
    fn test_new() {
        let m = Mount {
            fs_type: "overlay".to_string(),
            options: vec![
                "rw".to_string(),
                "lowerdir=/run/kata-containers/sandbox/layers/1181.erofs".to_string(),
                format!("{KATA_FS_OPT_LAYER_PREFIX}/dev/nbd2,ext4,rw"),
                KATA_FS_OPT_IS_LAYER.to_string(),
            ],
            ..Default::default()
        };

        let rootfs = KataOverlayRootfs::new("cid", &m).unwrap();

        assert_eq!(rootfs.guest_path, "/run/kata-containers/cid/rootfs");
        assert_eq!(
            rootfs.storage.mount_point,
            "/run/kata-containers/cid/rootfs"
        );
        assert_eq!(rootfs.storage.driver, "overlayfs");
        assert_eq!(rootfs.storage.source, "overlay");
        assert_eq!(rootfs.storage.fs_type, "overlay");
        assert_eq!(
            rootfs.storage.options,
            vec![
                "rw".to_string(),
                "lowerdir=/run/kata-containers/sandbox/layers/1181.erofs".to_string(),
            ]
        );

        let no_type = Mount {
            fs_type: String::new(),
            ..m.clone()
        };
        assert!(KataOverlayRootfs::new("cid", &no_type).is_err());
    }
}
