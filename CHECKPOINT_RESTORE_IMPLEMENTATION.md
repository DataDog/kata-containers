# Kata Containers Checkpoint/Restore Implementation Summary

## Overview
This document summarizes the complete implementation of checkpoint/restore functionality for Kata Containers, enabling container checkpointing using CRIU (Checkpoint/Restore in Userspace).

## Implementation Status: ✅ Complete Infrastructure

### 1. Guest Kernel Modifications
**File**: `tools/packaging/kernel/configs/fragments/common/namespaces.conf`
- **Change**: Added `CONFIG_EXPERT=y` and `CONFIG_CHECKPOINT_RESTORE=y`
- **Location**: Common kernel config fragment (applies to all architectures)
- **Rationale**: Checkpoint/restore is closely related to namespace functionality
- **Impact**: Guest kernel now supports CRIU operations
- **Verification**: Confirmed via `zcat /proc/config.gz | grep CONFIG_CHECKPOINT_RESTORE` in guest VM

### 2. CRIU Installation
- **Version**: Upgraded from 3.16.1 to 3.17.1
- **Location**: `/usr/sbin/criu` in guest image
- **Dependencies**: All verified (libprotobuf-c, libnl-3, libnet, libc)
- **Image**: `/usr/share/kata-containers/kata-containers-2025-12-03-10:10:39.555800518+0000-d9782598e`

### 3. Runtime Modifications (Go)

#### File: `src/runtime/virtcontainers/kata_agent.go`
- Added RPC handler constants:
  - `grpcCheckpointContainerRequest`
  - `grpcRestoreContainerRequest`
- Registered handlers in `installReqFunc()` to route checkpoint/restore RPCs to agent

#### File: `src/runtime/virtcontainers/sandbox.go`
- Modified `CheckpointContainer()` to allow paused containers (containerd behavior)
- Fixed checkpoint directory resolution for virtiofs shared mount
- Added debug logging for directory paths
- Uncommented and properly initialized `hostDir` and `guestDir` variables

### 4. Agent Modifications (Rust)

#### File: `src/agent/rustjail/src/container.rs`
- **State Validation**: Modified to accept `ContainerState::Paused` in addition to `Running`
- **Temporary Directory Workaround**: Implemented `/tmp/criu-checkpoint-*` to handle read-only virtiofs mount
- **Helper Function**: Added `copy_dir_all()` to transfer checkpoint files from temp to shared directory
- **Error Logging**: Enhanced CRIU stdout/stderr capture in error messages
- **CRIU Arguments**: Removed unsupported `--log-level` option for CRIU 3.17.1 compatibility

#### File: `src/agent/src/rpc.rs`
- Added debug logging in `do_checkpoint_container()` and `do_restore_container()`
- Logs container ID and guest checkpoint directory for troubleshooting

### 5. Configuration
**File**: `/etc/kata-containers/configuration.toml` (on remote host)
```toml
[runtime]
enable_checkpoint = true
enable_debug = true
guest_criu_path = "/usr/sbin/criu"
guest_checkpoint_dir = "/run/kata-containers/shared/containers/checkpoints"
host_checkpoint_dir = "/var/lib/kata-containers/checkpoints"
```

## Key Technical Decisions

### 1. Container State Handling
**Issue**: Containerd automatically pauses containers before checkpointing
**Solution**: Modified both runtime and agent to accept paused containers

### 2. Read-Only Filesystem
**Issue**: Virtiofs shared mount is read-only, preventing CRIU from writing directly
**Solution**: Use temporary `/tmp` directory for CRIU operations, then copy to shared mount

### 3. Directory Structure
- **Host**: `/run/kata-containers/shared/sandboxes/{sandbox_id}/shared/checkpoints/{container_id}/{checkpoint_id}/`
- **Guest**: `/run/kata-containers/shared/containers/checkpoints/{container_id}/{checkpoint_id}/`
- **Temp**: `/tmp/criu-checkpoint-{container_id}/images/` and `/tmp/criu-checkpoint-{container_id}/work/`

## Files Modified (Synced Locally)
- ✅ `src/agent/rustjail/src/container.rs`
- ✅ `src/agent/src/rpc.rs`
- ✅ `src/runtime/virtcontainers/sandbox.go`
- ✅ `src/runtime/virtcontainers/kata_agent.go`
- ✅ `tools/packaging/kernel/configs/fragments/common/namespaces.conf`

## Outstanding Issue
**Symptom**: CRIU executes but returns exit code 1 with empty stdout/stderr

**Possible Causes**:
1. Very early CRIU crash before output initialization
2. Output capture mechanism issue in Tokio async context
3. Missing kernel feature or VM environment incompatibility
4. CRIU encountering VM-specific limitation

**Next Steps for Resolution**:
1. Access VM serial console to view agent logs directly
2. Test CRIU manually inside the VM (not container) with simple command
3. Check VM dmesg for kernel messages during CRIU execution
4. Verify all CRIU kernel feature requirements are met

## Testing
```bash
# Create container
sudo nerdctl run -d --name test-container --runtime io.containerd.kata.v2 busybox sleep 3600

# Attempt checkpoint
sudo nerdctl checkpoint create test-container test-checkpoint --leave-running

# Current Result: CRIU exits with code 1, empty output
```

## Build Commands

### Rebuild Kernel
```bash
cd tools/packaging/kernel/kata-linux-6.12.47-173
make -j$(nproc)
sudo cp vmlinux /usr/share/kata-containers/vmlinux-6.12.47-173
```

### Rebuild Agent
```bash
cd src/agent
cargo build --release
# Then install into guest image
```

### Rebuild Runtime
```bash
cd src/runtime
make
sudo make install
```

## References
- [CRIU Documentation](https://criu.org/Documentation)
- [Kata Containers Checkpoint/Restore Guide](docs/how-to/how-to-use-kata-checkpoint-restore.md)
- [Containerd Task API](https://pkg.go.dev/github.com/containerd/containerd/api/services/tasks/v1)

---
**Implementation Date**: December 4, 2025
**Status**: Infrastructure Complete, Debugging CRIU Execution Issue
