// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

//! Kata overlays: rootfs layers described by `io.katacontainers.fs-opt.layer`
//! mount options.
//!
//! A snapshotter adds one `io.katacontainers.fs-opt.layer` option per layer to
//! the container rootfs mount. [`copy_layers_to_mounts`] turns them into OCI
//! mounts under `KATA_GUEST_SANDBOX_LAYERS_DIR` — the Go shim's
//! `copyLayersToMounts` — and [`claim_layers`] attaches each layer source to
//! the guest as a block device and mounts it there before the container
//! rootfs, which the guest agent assembles as an overlay of the layers. The
//! layers are hidden from the container.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use agent::Storage;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use hypervisor::device::device_manager::{do_handle_device, get_block_device_info, DeviceManager};
use hypervisor::device::{DeviceConfig, DeviceType};
use hypervisor::{BlockConfigModern, BlockDeviceAio};
use kata_sys_util::mount::{get_mount_options, get_mount_path};
use kata_types::mount::{
    Mount, KATA_FS_OPT_BLOCK_DEVICE_FILE, KATA_FS_OPT_IS_LAYER, KATA_FS_OPT_LAYER_PREFIX,
    KATA_GUEST_SANDBOX_LAYERS_DIR,
};
use nix::sys::stat::{self, SFlag};
use oci_spec::runtime as oci;
use tokio::sync::RwLock;

use crate::block_device::agent_storage_source_from_block_config;
use crate::volume::{
    utils::{KATA_MOUNT_BIND_TYPE, KATA_MOUNT_RBIND_TYPE},
    Volume,
};

/// Layer storage options: the `io.katacontainers.*` transport options (layer
/// descriptions, virtual volumes) and the bind flags are metadata, never mount
/// options — the guest filesystem would reject them.
const KATA_OPTION_PREFIX: &str = "io.katacontainers.";

pub(crate) fn filter_layer_storage_options(options: &[String]) -> Vec<String> {
    options
        .iter()
        .filter(|o| {
            !o.starts_with(KATA_OPTION_PREFIX)
                && !matches!(o.as_str(), KATA_MOUNT_BIND_TYPE | KATA_MOUNT_RBIND_TYPE)
        })
        .cloned()
        .collect()
}

/// Converts the `io.katacontainers.fs-opt.layer=<source>,<fstype>[,<option>...]`
/// options of the rootfs mounts into OCI mounts under
/// `KATA_GUEST_SANDBOX_LAYERS_DIR`, so the layer sources can be attached to the
/// guest and mounted there before the rootfs. Port of the Go shim's
/// `copyLayersToMounts`.
pub fn copy_layers_to_mounts(rootfs_mounts: &[Mount], spec: &mut oci::Spec) -> Result<()> {
    let mut layers = Vec::new();
    for m in rootfs_mounts {
        for o in &m.options {
            let Some(rest) = o.strip_prefix(KATA_FS_OPT_LAYER_PREFIX) else {
                continue;
            };
            layers.push(layer_mount(o, rest)?);
        }
    }
    if layers.is_empty() {
        return Ok(());
    }

    let mut mounts = spec.mounts().clone().unwrap_or_default();
    mounts.append(&mut layers);
    spec.set_mounts(Some(mounts));
    Ok(())
}

fn layer_mount(option: &str, rest: &str) -> Result<oci::Mount> {
    let fields: Vec<&str> = rest.split(',').collect();
    if fields.len() < 2 || fields[0].is_empty() || fields[1].is_empty() {
        return Err(anyhow!(
            "layer option {:?} needs <source>,<fstype>[,<option>...]",
            option
        ));
    }
    let name = Path::new(fields[0])
        .file_name()
        .filter(|n| !n.is_empty())
        .with_context(|| format!("layer option {:?}: source has no base name", option))?;

    let mut m = oci::Mount::default();
    m.set_destination(Path::new(KATA_GUEST_SANDBOX_LAYERS_DIR).join(name));
    m.set_typ(Some(fields[1].to_string()));
    m.set_source(Some(PathBuf::from(fields[0])));
    m.set_options(Some(fields[2..].iter().map(|o| o.to_string()).collect()));
    Ok(m)
}

/// Whether a mount describes a layer of an overlay filesystem: it is attached
/// as a block device, mounted in the guest before the rootfs, and hidden from
/// the container.
pub fn is_layer_mount(m: &oci::Mount) -> bool {
    get_mount_options(m.options())
        .iter()
        .any(|o| o == KATA_FS_OPT_IS_LAYER)
}

/// A layer of a kata overlay rootfs: the source of an `is-layer` mount,
/// attached to the guest as a block device and mounted in the guest at the
/// mount destination, before the container rootfs that overlays it.
pub(crate) struct LayerVolume {
    storage: Storage,
    device_id: String,
}

impl LayerVolume {
    pub async fn new(d: &RwLock<DeviceManager>, m: &oci::Mount) -> Result<Self> {
        let source = get_mount_path(m.source());
        let fstype = m.typ().clone().unwrap_or_default();
        if fstype.is_empty() {
            return Err(anyhow!(
                "layer mount {} has no filesystem type",
                m.destination().display()
            ));
        }
        let options = get_mount_options(m.options());
        let fstat =
            stat::stat(source.as_str()).with_context(|| format!("stat layer source {}", source))?;
        let file_type = SFlag::from_bits_truncate(fstat.st_mode);
        // Like the Go runtime's createBlockDevices: block-device sources are
        // attached directly, regular files only with the block_device=file
        // option.
        if file_type != SFlag::S_IFBLK
            && !(file_type == SFlag::S_IFREG
                && options.iter().any(|o| o == KATA_FS_OPT_BLOCK_DEVICE_FILE))
        {
            return Err(anyhow!(
                "layer source {} is neither a block device nor a block_device=file",
                source
            ));
        }
        let read_only = options.iter().any(|o| o == "ro");

        let blkdev_info = get_block_device_info(d).await;
        let config = BlockConfigModern {
            driver_option: blkdev_info.block_device_driver.clone(),
            path_on_host: source.clone(),
            is_readonly: read_only,
            blkdev_aio: BlockDeviceAio::new(&blkdev_info.block_device_aio),
            num_queues: blkdev_info.num_queues,
            queue_size: blkdev_info.queue_size,
            ..Default::default()
        };
        let device_info = do_handle_device(d, &DeviceConfig::BlockCfgModern(config))
            .await
            .context("attach layer device")?;
        let DeviceType::BlockModern(device) = device_info else {
            return Err(anyhow!("unexpected device for layer {}", source));
        };
        let device = device.lock().await.clone();

        let mut storage_options = filter_layer_storage_options(&options);
        if read_only && !storage_options.iter().any(|o| o == "ro") {
            storage_options.push("ro".to_string());
        }
        // The layer is mounted in the guest at its destination, like the Go
        // runtime's layer storages.
        let storage = Storage {
            driver: device.config.driver_option.clone(),
            source: agent_storage_source_from_block_config(&device.config)?,
            fs_type: fstype,
            options: storage_options,
            mount_point: m.destination().display().to_string(),
            ..Default::default()
        };

        Ok(Self {
            storage,
            device_id: device.device_id,
        })
    }
}

#[async_trait]
impl Volume for LayerVolume {
    fn get_volume_mount(&self) -> Result<Vec<oci::Mount>> {
        // Layers are guest-internal: the container never mounts them.
        Ok(vec![])
    }

    fn get_storage(&self) -> Result<Vec<Storage>> {
        Ok(vec![self.storage.clone()])
    }

    fn get_device_id(&self) -> Result<Option<String>> {
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

/// Claims the `is-layer` mounts of the spec: attaches each source to the
/// guest as a block device and returns volumes whose storages mount them at
/// their destinations, before the rootfs. The claimed mounts are removed from
/// the spec, so no container-visible volume is created for them.
pub async fn claim_layers(
    d: &RwLock<DeviceManager>,
    spec: &mut oci::Spec,
) -> Result<Vec<Arc<dyn Volume>>> {
    let Some(mounts) = spec.mounts() else {
        return Ok(vec![]);
    };
    let layer_mounts: Vec<oci::Mount> = mounts
        .iter()
        .filter(|m| is_layer_mount(m))
        .cloned()
        .collect();
    if layer_mounts.is_empty() {
        return Ok(vec![]);
    }

    if let Some(mounts) = spec.mounts_mut() {
        mounts.retain(|m| !is_layer_mount(m));
    }

    let mut volumes: Vec<Arc<dyn Volume>> = Vec::with_capacity(layer_mounts.len());
    for m in &layer_mounts {
        match LayerVolume::new(d, m).await {
            Ok(v) => volumes.push(Arc::new(v)),
            Err(e) => {
                // The caller never receives these volumes and cannot clean
                // them up.
                for v in volumes.iter().rev() {
                    if let Err(err) = v.cleanup(d).await {
                        warn!(sl!(), "failed to clean up layer volume: {:?}", err);
                    }
                }
                return Err(e);
            }
        }
    }
    Ok(volumes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAYERS_DIR: &str = "/run/kata-containers/sandbox/layers";

    fn rootfs_mount(options: &[String]) -> Mount {
        Mount {
            fs_type: "overlay".to_string(),
            options: options.iter().map(|o| o.to_string()).collect(),
            ..Default::default()
        }
    }

    fn oci_mount(destination: &str, typ: &str, source: &str, options: &[&str]) -> oci::Mount {
        let mut m = oci::Mount::default();
        m.set_destination(PathBuf::from(destination));
        m.set_typ(Some(typ.to_string()));
        m.set_source(Some(PathBuf::from(source)));
        m.set_options(Some(options.iter().map(|o| o.to_string()).collect()));
        m
    }

    fn spec_with_mounts(mounts: Vec<oci::Mount>) -> oci::Spec {
        let mut spec = oci::Spec::default();
        spec.set_mounts(Some(mounts));
        spec
    }

    // (destination, typ, source, options) of the injected layer mounts, or an
    // error. The pre-existing spec mounts must be kept in front.
    type Want = Result<Vec<(String, String, String, Vec<String>)>, ()>;

    fn want_mounts(spec: &oci::Spec) -> Vec<(String, String, String, Vec<String>)> {
        spec.mounts()
            .clone()
            .unwrap_or_default()
            .iter()
            .map(|m| {
                (
                    m.destination().display().to_string(),
                    m.typ().clone().unwrap_or_default(),
                    m.source().clone().unwrap_or_default().display().to_string(),
                    m.options().clone().unwrap_or_default(),
                )
            })
            .collect()
    }

    #[test]
    fn test_copy_layers_to_mounts() {
        let upper_option = format!("{KATA_FS_OPT_LAYER_PREFIX}/dev/nbd2,ext4,rw");
        let image_option = format!(
            "{KATA_FS_OPT_LAYER_PREFIX}/var/lib/nydus/1181.erofs,erofs,ro,io.katacontainers.fs-opt.is-layer"
        );
        let cases: Vec<(&str, Vec<String>, Want)> = vec![
            (
                "no layer options",
                vec!["rw".to_string(), "lowerdir=/a".to_string()],
                Ok(vec![(
                    "/proc".to_string(),
                    "proc".to_string(),
                    "proc".to_string(),
                    vec![],
                )]),
            ),
            (
                "one layer with options",
                vec!["rw".to_string(), upper_option.clone()],
                Ok(vec![
                    (
                        "/proc".to_string(),
                        "proc".to_string(),
                        "proc".to_string(),
                        vec![],
                    ),
                    (
                        format!("{LAYERS_DIR}/nbd2"),
                        "ext4".to_string(),
                        "/dev/nbd2".to_string(),
                        vec!["rw".to_string()],
                    ),
                ]),
            ),
            (
                "several layers",
                vec![upper_option, image_option],
                Ok(vec![
                    (
                        "/proc".to_string(),
                        "proc".to_string(),
                        "proc".to_string(),
                        vec![],
                    ),
                    (
                        format!("{LAYERS_DIR}/nbd2"),
                        "ext4".to_string(),
                        "/dev/nbd2".to_string(),
                        vec!["rw".to_string()],
                    ),
                    (
                        format!("{LAYERS_DIR}/1181.erofs"),
                        "erofs".to_string(),
                        "/var/lib/nydus/1181.erofs".to_string(),
                        vec!["ro".to_string(), KATA_FS_OPT_IS_LAYER.to_string()],
                    ),
                ]),
            ),
            (
                "missing fields",
                vec![format!("{KATA_FS_OPT_LAYER_PREFIX}/dev/nbd2")],
                Err(()),
            ),
            (
                "empty fstype",
                vec![format!("{KATA_FS_OPT_LAYER_PREFIX}/dev/nbd2,")],
                Err(()),
            ),
            (
                "empty source",
                vec![format!("{KATA_FS_OPT_LAYER_PREFIX},ext4")],
                Err(()),
            ),
        ];

        for (name, options, want) in cases {
            let mut spec = spec_with_mounts(vec![oci_mount("/proc", "proc", "proc", &[])]);
            let result = copy_layers_to_mounts(&[rootfs_mount(&options)], &mut spec);
            match want {
                Ok(want) => {
                    result.unwrap_or_else(|e| panic!("{}: {:#}", name, e));
                    assert_eq!(want_mounts(&spec), want, "{}", name);
                }
                Err(()) => {
                    assert!(result.is_err(), "{}", name);
                }
            }
        }
    }

    #[test]
    fn test_copy_layers_no_mounts_in_spec() {
        let mut spec = oci::Spec::default();
        // A spec with no mounts must still end up with the injected layer.
        spec.set_mounts(None);
        copy_layers_to_mounts(
            &[rootfs_mount(&[format!(
                "{KATA_FS_OPT_LAYER_PREFIX}/dev/nbd2,ext4,rw"
            )])],
            &mut spec,
        )
        .unwrap();
        assert_eq!(
            want_mounts(&spec),
            vec![(
                format!("{LAYERS_DIR}/nbd2"),
                "ext4".to_string(),
                "/dev/nbd2".to_string(),
                vec!["rw".to_string()],
            )]
        );
    }

    #[test]
    fn test_is_layer_mount() {
        let cases = [
            ("is-layer", vec!["rw", KATA_FS_OPT_IS_LAYER], true),
            ("no option", vec!["rw"], false),
            ("other option", vec!["rw", "ro"], false),
            ("no options", vec![], false),
        ];
        for (name, options, want) in cases {
            let m = oci_mount("/a", "ext4", "/dev/b", &options);
            assert_eq!(is_layer_mount(&m), want, "{name}");
        }
    }

    #[test]
    fn test_filter_layer_storage_options() {
        let options: Vec<String> = vec![
            "rw".to_string(),
            "bind".to_string(),
            "rbind".to_string(),
            format!("{KATA_FS_OPT_LAYER_PREFIX}/dev/nbd2,ext4"),
            KATA_FS_OPT_IS_LAYER.to_string(),
            KATA_FS_OPT_BLOCK_DEVICE_FILE.to_string(),
            "io.katacontainers.volume.eyJ2b2x1bWVfdHlwZSI6".to_string(),
        ];
        assert_eq!(
            filter_layer_storage_options(&options),
            vec!["rw".to_string()]
        );
    }
}
