# How to use Kata Containers checkpoint/restore

Kata Containers can leverage [CRIU](https://criu.org/) to checkpoint a running
container inside the guest VM and later restore it. This guide explains how to
enable the feature and exercise it with `nerdctl`.

> **Status:** experimental. The feature currently targets the Go shim
> (`io.containerd.kata.v2`) and `containerd`. Incremental checkpoints, GPU
> workloads, and non-Linux hosts are not yet supported.

## Prerequisites

1. Kata Containers 3.x with the Go runtime (`io.containerd.kata.v2`).
2. `containerd` 1.6+ (or any consumer that exposes the Task gRPC API).
3. A guest image that contains a CRIU binary (minimum version 3.17 is
   recommended).

## Enable checkpointing in Kata

Checkpointing is disabled by default. To turn it on:

1. Edit your Kata configuration (for example `/etc/kata-containers/configuration-qemu.toml`)
   and set the following `[runtime]` options:

   ```toml
   [runtime]
   enable_checkpoint = true
   guest_criu_path = "/usr/sbin/criu"
   guest_checkpoint_dir = "/run/kata-containers/checkpoints"
   host_checkpoint_dir = "/var/lib/kata-containers/checkpoints"
   ```

   `guest_criu_path` and the guest/host checkpoint directories may be adjusted
   to match your environment. The host path does not need to be shared with the
   workload—Kata uses it purely for staging CRIU images.

2. Rebuild the guest rootfs so that CRIU is present inside the VM. When using
   osbuilder, the following is sufficient because the CRIU package is added
   automatically once `enable_checkpoint = true`:

   ```bash
   $ cd <kata-repo>/tools/osbuilder
   $ sudo -E AGENT_INIT=yes ./rootfs-builder/rootfs.sh ubuntu
   ```

   (Replace `ubuntu` with the distro you normally build.)

3. Restart the Kata shim / containerd components so they pick up the new
   configuration and rootfs.

## Checkpoint a container

The example below uses `nerdctl`, but any containerd client that drives the
Task API works the same way.

```bash
# Run a sample workload with Kata
$ nerdctl run -d --runtime io.containerd.kata.v2 --name kata-ckpt busybox sleep 600

# Create a directory on the host where CRIU images will be exported
$ sudo mkdir -p /var/lib/kata/checkpoints/kata-ckpt

# Instruct containerd to checkpoint the task and leave it running
$ nerdctl container checkpoint \
    --leave-running \
    --image-dir /var/lib/kata/checkpoints/kata-ckpt \
    kata-ckpt
```

The command above causes containerd to call `TaskService.Checkpoint` with
`CheckpointTaskRequest.path = /var/lib/kata/checkpoints/kata-ckpt`. The Kata
shim copies the CRIU bundle produced inside the VM to that directory and emits
the standard `TaskCheckpointed` event.

## Restore a container

To restore, containerd invokes `CreateTaskRequest.checkpoint` pointing at a host
directory that contains a CRIU bundle. `nerdctl container restore` drives that
flow:

```bash
# Stop the original container (if it was left running)
$ nerdctl rm -f kata-ckpt

# Restore the checkpoint into a new container
$ nerdctl container restore \
    --runtime io.containerd.kata.v2 \
    --image-dir /var/lib/kata/checkpoints/kata-ckpt \
    kata-ckpt-restored
```

During `restore`, Kata stages the CRIU bundle inside the sandbox shared
directory and asks the agent to restore the container before running OCI
post-start hooks.

## Known limitations

- Incremental checkpoints (`parent_checkpoint`) are rejected.
- Only the Go shim (`io.containerd.kata.v2`) speaks the protocol today.
- The guest rootfs must contain CRIU and all of its dependencies.
- CRIU’s own limitations still apply (e.g. no GPU device state migration).

For more details on the implementation and current constraints see
[docs/Limitations.md](../Limitations.md#checkpoint-and-restore).

