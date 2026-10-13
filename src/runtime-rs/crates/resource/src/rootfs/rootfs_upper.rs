// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

use std::{path::Path, sync::Arc};

use agent::Storage;
use anyhow::{Context, Result};
use hypervisor::device::device_manager::DeviceManager;
use kata_sys_util::mount::{get_mount_options, get_mount_path};
use kata_types::{
    annotations::KATA_ANNO_VOLUME_ROOTFS_UPPER, device::DRIVER_OVERLAYFS_TYPE,
    mount::KATA_VOLUME_OVERLAYFS_CREATE_DIR,
};
use oci_spec::runtime as oci;
use tokio::sync::RwLock;

use super::{Rootfs, ROOTFS, TYPE_OVERLAY_FS};
use crate::volume::{
    direct_volumes::{rawblock_volume::RawblockVolume, volume_mount_info, KATA_DIRECT_VOLUME_TYPE},
    Volume,
};

fn upper_path(spec: &oci::Spec) -> Option<String> {
    spec.annotations()
        .as_ref()
        .and_then(|a| a.get(KATA_ANNO_VOLUME_ROOTFS_UPPER))
        .filter(|u| !u.is_empty())
        .cloned()
}

/// Hotplugs the direct-assigned block volume named by the rootfs-upper annotation and
/// removes its mount from the spec, so the generic volume handlers never see it.
pub async fn claim(
    d: &RwLock<DeviceManager>,
    sid: &str,
    spec: &mut oci::Spec,
) -> Result<Option<Arc<dyn Volume>>> {
    let Some(upper) = upper_path(spec) else {
        return Ok(None);
    };
    let Some(m) = spec
        .mounts()
        .as_ref()
        .and_then(|mounts| mounts.iter().find(|m| m.destination() == Path::new(&upper)))
        .cloned()
    else {
        warn!(sl!(), "no mount at rootfs upper {}", upper);
        return Ok(None);
    };

    let mut info = volume_mount_info(&get_mount_path(m.source()))
        .with_context(|| format!("get direct volume info for rootfs upper {upper}"))?;
    // Producers such as the ZFS CSI driver declare "block"; it is handled as a raw block volume.
    info.volume_type = KATA_DIRECT_VOLUME_TYPE.to_string();
    let read_only = get_mount_options(m.options()).iter().any(|o| o == "ro");
    let volume = RawblockVolume::new(d, &m, &info, read_only, sid)
        .await
        .context("create rootfs upper volume")?;

    if let Some(mounts) = spec.mounts_mut() {
        mounts.retain(|m| m.destination() != Path::new(&upper));
    }
    Ok(Some(Arc::new(volume)))
}

/// Mounts the claimed upper volume first and stacks a host-shared rootfs under an
/// overlay whose upper/work directories live on it.
pub async fn apply(
    cid: &str,
    rootfs: Option<&Arc<dyn Rootfs>>,
    upper_volume: &dyn Volume,
    spec: &mut oci::Spec,
    storages: &mut Vec<Storage>,
) -> Result<()> {
    let Some(upper) = upper_path(spec) else {
        return Ok(());
    };
    let mut upper_storage = upper_volume
        .get_storage()?
        .into_iter()
        .next()
        .context("rootfs upper volume has no storage")?;

    if let Some(rootfs) = rootfs.filter(|r| r.is_host_shared()) {
        let overlay = overlay_storage(cid, &rootfs.get_guest_rootfs_path().await?, &upper);
        if let Some(root) = spec.root_mut() {
            root.set_path(overlay.mount_point.clone().into());
        }
        storages.insert(0, overlay);
    }

    upper_storage.mount_point = upper;
    storages.insert(0, upper_storage);
    Ok(())
}

fn overlay_storage(cid: &str, lower: &str, upper: &str) -> Storage {
    let upper_dir = format!("{upper}/upper");
    let work_dir = format!("{upper}/work");
    Storage {
        driver: DRIVER_OVERLAYFS_TYPE.to_string(),
        driver_options: vec![
            format!("{KATA_VOLUME_OVERLAYFS_CREATE_DIR}={upper_dir}"),
            format!("{KATA_VOLUME_OVERLAYFS_CREATE_DIR}={work_dir}"),
        ],
        source: TYPE_OVERLAY_FS.to_string(),
        fs_type: TYPE_OVERLAY_FS.to_string(),
        // The trailing "/" makes the guest mount the rootfs virtio-fs submount
        // (announce_submounts) before overlayfs checks the lower.
        options: vec![
            format!("lowerdir={lower}/"),
            format!("upperdir={upper_dir}"),
            format!("workdir={work_dir}"),
            "index=off".to_string(),
        ],
        mount_point: format!("/run/kata-containers/{cid}/{ROOTFS}"),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::HashMap;

    const LOWER: &str = "/run/kata-containers/shared/containers/cid/rootfs";
    const UPPER: &str = "/run/kata-containers/sandbox/layers/rootfs-upper";

    fn storage(name: &str) -> Storage {
        Storage {
            source: name.to_string(),
            mount_point: format!("/run/kata-containers/shared/containers/{name}"),
            ..Default::default()
        }
    }

    struct FakeVolume;

    #[async_trait]
    impl Volume for FakeVolume {
        fn get_volume_mount(&self) -> Result<Vec<oci::Mount>> {
            Ok(vec![])
        }
        fn get_storage(&self) -> Result<Vec<Storage>> {
            Ok(vec![storage("upper")])
        }
        fn get_device_id(&self) -> Result<Option<String>> {
            Ok(Some("dev0".to_string()))
        }
        async fn cleanup(&self, _: &RwLock<DeviceManager>) -> Result<()> {
            Ok(())
        }
    }

    struct FakeRootfs;

    #[async_trait]
    impl Rootfs for FakeRootfs {
        async fn get_guest_rootfs_path(&self) -> Result<String> {
            Ok(LOWER.to_string())
        }
        async fn get_rootfs_mount(&self) -> Result<Vec<oci::Mount>> {
            Ok(vec![])
        }
        async fn get_storage(&self) -> Option<Vec<Storage>> {
            None
        }
        async fn cleanup(&self, _: &RwLock<DeviceManager>) -> Result<()> {
            Ok(())
        }
        async fn get_device_id(&self) -> Result<Option<String>> {
            Ok(None)
        }
        fn is_host_shared(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn test_apply() {
        let mut spec = oci::Spec::default();
        spec.set_annotations(Some(HashMap::from([(
            KATA_ANNO_VOLUME_ROOTFS_UPPER.to_string(),
            UPPER.to_string(),
        )])));
        let mut storages = vec![storage("other")];
        let rootfs: Arc<dyn Rootfs> = Arc::new(FakeRootfs);

        apply("cid", Some(&rootfs), &FakeVolume, &mut spec, &mut storages)
            .await
            .unwrap();

        assert_eq!(
            spec.root().as_ref().unwrap().path(),
            Path::new("/run/kata-containers/cid/rootfs")
        );
        let overlay = Storage {
            driver: "overlayfs".to_string(),
            driver_options: vec![
                format!("io.katacontainers.volume.overlayfs.create_directory={UPPER}/upper"),
                format!("io.katacontainers.volume.overlayfs.create_directory={UPPER}/work"),
            ],
            source: "overlay".to_string(),
            fs_type: "overlay".to_string(),
            options: vec![
                format!("lowerdir={LOWER}/"),
                format!("upperdir={UPPER}/upper"),
                format!("workdir={UPPER}/work"),
                "index=off".to_string(),
            ],
            mount_point: "/run/kata-containers/cid/rootfs".to_string(),
            ..Default::default()
        };
        let upper = Storage {
            mount_point: UPPER.to_string(),
            ..storage("upper")
        };
        assert_eq!(storages, [upper, overlay, storage("other")]);
    }
}
