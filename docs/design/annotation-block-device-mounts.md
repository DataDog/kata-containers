# Annotation-Based Block Device Mounting

## Motivation

Kubernetes `volumeDevices` exposes block devices as raw `/dev/` paths inside containers.
In a Kata Containers VM this means the block device is hotplugged and presented to the
guest as a character/block device node, but no filesystem is mounted. The application must
format and mount the device itself.

Many workloads (databases, caches, ML training) need a *mounted filesystem*, not a raw
device. Today, achieving this requires either:

1. Using `volumeMounts` with Filesystem-mode PVCs, which routes through virtio-fs and
   loses the performance benefits of direct block access.
2. Using the [direct-assigned volume](direct-blk-device-assignment.md) mechanism, which
   requires a CSI driver integration and the `kata-ctl direct-volume` CLI.

Neither option works well when:
- The CSI driver cannot be modified (e.g. upstream AWS EBS CSI).
- The volume must remain in Block mode for other consumers in the same cluster.
- A lightweight, annotation-only solution is preferred over CSI plugin changes.

## Proposed Solution

A new pod annotation `io.katacontainers.volume.block-mounts` lets users declare that
specific `volumeDevices` should be mounted as filesystems inside the guest VM. The Kata
runtime intercepts these devices during container creation, creates `Storage` gRPC objects
for the kata-agent, and adds bind mounts to the OCI spec.

The block devices are still hotplugged through the standard Kubernetes `volumeDevices`
path. This annotation only changes what happens *after* hotplug: instead of passing the
device as a raw `/dev/` node, the runtime instructs the agent to mount it.

### Annotation Format

The annotation value is a JSON object. Keys are container device paths (matching
`volumeDevices[].devicePath`), values are mount configuration objects:

```json
{
  "/dev/vdb": {
    "mount": "/data",
    "fstype": "ext4",
    "options": ["rw", "noatime"],
    "fsGroup": 1000
  },
  "/dev/vdc": {
    "mount": "/cache",
    "fstype": "xfs"
  }
}
```

| Field     | Type     | Required | Default  | Description |
|-----------|----------|----------|----------|-------------|
| `mount`   | string   | yes      | -        | Absolute path where the filesystem is mounted inside the container |
| `fstype`  | string   | no       | `ext4`   | Filesystem type. Must be `ext4` or `xfs` |
| `options` | []string | no       | `["rw"]` | Mount options passed to the agent |
| `fsGroup` | int64    | no       | -        | If set, ownership of the mount is changed to this GID |

### Validation Rules

- Device paths must start with `/dev/`.
- Mount destinations must be absolute paths.
- Filesystem type must be `ext4`, `xfs`, or empty (defaults to `ext4`).
- Every annotated device must match a `volumeDevices` entry in the container spec.
- Duplicate container device paths are rejected.

## Implementation Details

### Runtime (Go) - `kata_agent.go`

The implementation adds three stages to container creation:

#### 1. Device Filtering (`appendDevices`)

When building the gRPC device list, the runtime parses the block mount annotation and
skips any device whose `ContainerPath` appears in the annotation. This prevents the
device from being passed as a raw `/dev/` node to the guest.

#### 2. Storage Creation (`createAnnotationBlockStorages`)

For each annotated device, the runtime:

1. Looks up the device in the device manager.
2. Delegates driver selection to `handleBlockVolume()`, which inspects the `BlockDrive`
   struct fields (e.g. `Pmem`, `PCIPath`, `DevNo`) to determine the correct storage
   driver (`blk`, `nvdimm`, `virtio-scsi`, etc.). This avoids duplicating the
   driver-selection logic.
3. If the host-side block device has no filesystem (detected via `blkid`), it formats the
   device using `mkfs.<fstype>`. This handles fresh ephemeral volumes (e.g. unformatted
   EBS volumes) where the guest rootfs does not ship filesystem tools.
4. Constructs a `Storage` gRPC object with the filesystem type, mount options, and a
   base64-encoded guest mount point under the sandbox storage directory.
5. Adds a bind mount to the OCI spec pointing from the guest mount point to the
   user-specified container path.

#### 3. OCI Spec Cleanup (`removeDevicesFromOCISpec`)

After processing, the annotated devices are removed from `spec.Linux.Devices` since they
are no longer raw device nodes.

### Driver Selection

The runtime delegates to `handleBlockVolume()` rather than reading
`HypervisorConfig.BlockDeviceDriver` directly. This function uses struct-based detection:

```
BlockDrive.Pmem == true       -> nvdimm driver
BlockDeviceDriver == VirtioCCW -> blk-ccw driver
BlockDeviceDriver == VirtioBlk -> blk driver
BlockDeviceDriver == VirtioMmio -> mmio-blk driver
BlockDeviceDriver == VirtioSCSI -> scsi driver
```

This ensures correct driver selection for all device types, including pmem devices that
may be configured alongside a different default block driver.

### Host-Side Formatting

`formatBlockDeviceIfNeeded()` runs on the host before the device reaches the guest:

1. Runs `blkid -p <device>` to check for an existing filesystem.
2. If no filesystem is found, runs `mkfs.<fstype> <device>`.
3. Only `ext4` and `xfs` are allowed (enforced by annotation validation).

This is necessary because fresh block volumes (e.g. newly provisioned EBS, local SSDs)
arrive unformatted, and the guest rootfs typically does not include `mkfs` or `blkid`.

### Annotation Lookup

The runtime checks container-level annotations first, falling back to sandbox-level
annotations. This allows the annotation to be set at either the pod or container level.

## End User Interface

### Kubernetes Pod Spec

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: block-mount-example
  annotations:
    io.katacontainers.volume.block-mounts: |
      {"/dev/xvda": {"mount": "/data", "fstype": "ext4", "options": ["rw", "noatime"]}}
spec:
  runtimeClassName: kata
  containers:
  - name: app
    image: myapp:latest
    volumeDevices:
    - name: data-vol
      devicePath: /dev/xvda
  volumes:
  - name: data-vol
    persistentVolumeClaim:
      claimName: my-block-pvc
---
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: my-block-pvc
spec:
  accessModes:
    - ReadWriteOncePod
  volumeMode: Block
  storageClassName: ebs-sc
  resources:
    requests:
      storage: 100Gi
```

### containerd Configuration

The containerd config must allow Kata annotations to pass through (this is typically
already configured for Kata deployments):

```toml
[plugins."io.containerd.grpc.v1.cri".containerd.runtimes.kata]
  runtime_type = "io.containerd.kata.v2"
  pod_annotations = ["io.katacontainers.*"]
```

## Comparison with Direct-Assigned Volumes

| Aspect | Block Mount Annotation | Direct-Assigned Volume |
|--------|----------------------|----------------------|
| CSI driver changes | None | Required (`kata-ctl direct-volume add`) |
| Volume mode | Block (`volumeDevices`) | Filesystem (`volumeMounts`) |
| Resize support | No | Yes (via `kata-ctl direct-volume resize`) |
| Stats collection | No | Yes (via `kata-ctl direct-volume stats`) |
| Configuration | Pod annotation | `mountInfo.json` on host filesystem |
| Use case | Simple block-to-mount conversion | Full CSI integration with lifecycle management |

## Limitations

1. Only `ext4` and `xfs` filesystem types are supported.
2. Volume resize and stats collection are not supported (use direct-assigned volumes for
   these features).
3. The annotation applies to the Go runtime (`runtime-go`) only. Runtime-rs support is
   planned.
4. Host-side formatting requires `blkid` and `mkfs.<fstype>` to be available on the host.
