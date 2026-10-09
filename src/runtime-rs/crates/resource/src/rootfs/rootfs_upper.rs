// Copyright (c) 2026 Datadog, Inc.
//
// SPDX-License-Identifier: Apache-2.0
//

use std::{path::Path, sync::Arc};

use agent::Storage;
use anyhow::Result;
use kata_types::{
    annotations::KATA_ANNO_VOLUME_ROOTFS_UPPER, device::DRIVER_OVERLAYFS_TYPE,
    mount::KATA_VOLUME_OVERLAYFS_CREATE_DIR,
};
use oci_spec::runtime as oci;

use super::{Rootfs, ROOTFS, TYPE_OVERLAY_FS};
use crate::volume::Volume;

/// Mounts the block volume named by the rootfs-upper annotation first, hides it from the
/// container, and stacks a host-shared rootfs under an overlay whose upper/work live on it.
pub async fn apply(
    cid: &str,
    rootfs: Option<&Arc<dyn Rootfs>>,
    volumes: &[Arc<dyn Volume>],
    spec: &mut oci::Spec,
    storages: &mut Vec<Storage>,
) -> Result<()> {
    let Some(upper) = spec
        .annotations()
        .as_ref()
        .and_then(|a| a.get(KATA_ANNO_VOLUME_ROOTFS_UPPER))
        .filter(|u| !u.is_empty())
        .cloned()
    else {
        return Ok(());
    };

    let upper_storage = volumes
        .iter()
        .find_map(|v| block_storage_at(v.as_ref(), &upper).transpose())
        .transpose()?;
    let Some(mut upper_storage) = upper_storage else {
        warn!(sl!(), "no block volume mounted at rootfs upper {}", upper);
        return Ok(());
    };

    if let Some(i) = storages.iter().position(|s| *s == upper_storage) {
        storages.remove(i);
    }
    if let Some(mounts) = spec.mounts_mut() {
        mounts.retain(|m| m.destination() != Path::new(&upper));
    }

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

fn block_storage_at(v: &dyn Volume, dest: &str) -> Result<Option<Storage>> {
    if v.get_device_id()?.is_none()
        || !v
            .get_volume_mount()?
            .iter()
            .any(|m| m.destination() == Path::new(dest))
    {
        return Ok(None);
    }
    Ok(match v.get_storage()?.as_slice() {
        [s] => Some(s.clone()),
        _ => None,
    })
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
    use hypervisor::device::device_manager::DeviceManager;
    use std::collections::HashMap;
    use tokio::sync::RwLock;

    const LOWER: &str = "/run/kata-containers/shared/containers/cid/rootfs";
    const UPPER: &str = "/rootfs-upper";

    #[derive(Clone)]
    struct FakeVolume {
        dest: &'static str,
        device_id: Option<&'static str>,
    }

    impl FakeVolume {
        fn mount(&self) -> oci::Mount {
            let mut m = oci::Mount::default();
            m.set_destination(self.dest.into());
            m
        }
        fn storage(&self) -> Storage {
            Storage {
                source: self.dest.to_string(),
                mount_point: format!("/run/kata-containers/shared/containers{}", self.dest),
                ..Default::default()
            }
        }
    }

    #[async_trait]
    impl Volume for FakeVolume {
        fn get_volume_mount(&self) -> Result<Vec<oci::Mount>> {
            Ok(vec![self.mount()])
        }
        fn get_storage(&self) -> Result<Vec<Storage>> {
            Ok(vec![self.storage()])
        }
        fn get_device_id(&self) -> Result<Option<String>> {
            Ok(self.device_id.map(String::from))
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

    async fn run(vols: &[FakeVolume]) -> (oci::Spec, Vec<Storage>) {
        let mut spec = oci::Spec::default();
        spec.set_annotations(Some(HashMap::from([(
            KATA_ANNO_VOLUME_ROOTFS_UPPER.to_string(),
            UPPER.to_string(),
        )])));
        spec.set_mounts(Some(vols.iter().map(FakeVolume::mount).collect()));
        let mut storages: Vec<Storage> = vols.iter().map(FakeVolume::storage).collect();
        let volumes: Vec<Arc<dyn Volume>> = vols
            .iter()
            .map(|v| Arc::new(v.clone()) as Arc<dyn Volume>)
            .collect();
        let rootfs: Arc<dyn Rootfs> = Arc::new(FakeRootfs);
        apply("cid", Some(&rootfs), &volumes, &mut spec, &mut storages)
            .await
            .unwrap();
        (spec, storages)
    }

    fn dests(spec: &oci::Spec) -> Vec<&Path> {
        spec.mounts()
            .as_ref()
            .unwrap()
            .iter()
            .map(|m| m.destination().as_path())
            .collect()
    }

    #[tokio::test]
    async fn test_apply_ignores_non_block_volume() {
        let vols = [
            FakeVolume {
                dest: UPPER,
                device_id: None,
            },
            FakeVolume {
                dest: "/other",
                device_id: Some("dev0"),
            },
        ];
        let (spec, storages) = run(&vols).await;
        assert_eq!(dests(&spec), [Path::new(UPPER), Path::new("/other")]);
        assert_eq!(storages, [vols[0].storage(), vols[1].storage()]);
        assert_eq!(spec.root().as_ref().unwrap().path(), Path::new("rootfs"));
    }

    #[tokio::test]
    async fn test_apply() {
        let vols = [
            FakeVolume {
                dest: "/a",
                device_id: Some("dev0"),
            },
            FakeVolume {
                dest: UPPER,
                device_id: Some("dev1"),
            },
        ];
        let (spec, storages) = run(&vols).await;

        assert_eq!(dests(&spec), [Path::new("/a")]);
        assert_eq!(
            spec.root().as_ref().unwrap().path(),
            Path::new("/run/kata-containers/cid/rootfs")
        );
        let overlay = Storage {
            driver: "overlayfs".to_string(),
            driver_options: vec![
                "io.katacontainers.volume.overlayfs.create_directory=/rootfs-upper/upper"
                    .to_string(),
                "io.katacontainers.volume.overlayfs.create_directory=/rootfs-upper/work"
                    .to_string(),
            ],
            source: "overlay".to_string(),
            fs_type: "overlay".to_string(),
            options: vec![
                format!("lowerdir={LOWER}/"),
                "upperdir=/rootfs-upper/upper".to_string(),
                "workdir=/rootfs-upper/work".to_string(),
                "index=off".to_string(),
            ],
            mount_point: "/run/kata-containers/cid/rootfs".to_string(),
            ..Default::default()
        };
        let upper = Storage {
            mount_point: UPPER.to_string(),
            ..vols[1].storage()
        };
        assert_eq!(storages, [upper, overlay, vols[0].storage()]);
    }
}
