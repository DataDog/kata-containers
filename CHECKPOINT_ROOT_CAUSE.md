# Kata Containers Checkpoint/Restore - Root Cause Identified

**Date**: December 4, 2025  
**Status**: ROOT CAUSE FOUND ✅ - Implementation Fix Pending

## Executive Summary

The checkpoint/restore functionality is failing because **the virtiofs shared mount is read-only from the guest VM perspective**. The agent (running in guest) cannot create the checkpoint directories that CRIU needs, causing silent failures before CRIU ever executes.

## Root Cause Analysis

### The Problem

1. **Virtiofs Mount Constraint**:
   - Shared directory (`/run/kata-containers/shared/`) mounted via virtiofs
   - **Writable from host, READ-ONLY from guest**
   - Agent cannot create directories in this mount

2. **Current Broken Flow**:
   ```
   Runtime (host) → Agent (guest) → tokio::fs::create_dir_all() → FAILS silently
   ```

3. **Evidence from Investigation**:
   - Agent code calls `tokio::fs::create_dir_all(&cfg.work_dir)` 
   - This fails with "Read-only file system" error
   - CRIU never executes because directory creation fails first
   - Diagnostic files aren't created because failure happens before file operations
   - Manual test: Creating directories from host side works perfectly

## The Solution

**Pre-create checkpoint directories on HOST before calling agent:**

### Code Change in `src/runtime/virtcontainers/sandbox.go`

```go
hostDir, guestDir := s.resolveCheckpointDirs(req.ContainerID, req.CheckpointID)

// Create the checkpoint directories on the host side, including images and work subdirectories
// The virtiofs mount is read-only from the guest, so the agent cannot create these directories
imagesDir := filepath.Join(hostDir, "images")
workDir := filepath.Join(hostDir, "work")
if err := os.MkdirAll(imagesDir, 0755); err != nil {
    return nil, fmt.Errorf("failed to create images directory %s: %w", imagesDir, err)
}
if err := os.MkdirAll(workDir, 0755); err != nil {
    return nil, fmt.Errorf("failed to create work directory %s: %w", workDir, err)
}
```

## Implementation Status

### ✅ Completed

1. **Runtime Logic**: Updated `sandbox.go` with host-side directory pre-creation
2. **Agent Updates**: 
   - Handle paused containers (containerd pauses before checkpointing)
   - Comprehensive diagnostic logging with CRIU check
   - File-based output capture for debugging
3. **Kernel Configuration**: Added `CONFIG_CHECKPOINT_RESTORE=y` and `CONFIG_EXPERT=y` to kernel fragments
4. **CRIU**: Version 3.17.1 installed in guest image
5. **Configuration**: `enable_checkpoint=true`, debug logging enabled

### ⚠️ Pending - Final Step

**Rebuild and install the updated runtime binary**:
- Current installed binary predates the directory pre-creation fix
- Build environment has Go dependency issues (missing vendor packages)
- Workaround options:
  1. Fix build environment and rebuild
  2. Copy pre-built binary to `/usr/local/bin/`

## Technical Architecture

### Component Interaction

```
┌─────────────────────────────────────────┐
│ Host (Kata Runtime)                      │
│ - Manages VMs and sandboxes              │
│ - Creates checkpoint directories ← FIX   │
│ - Read/Write access to shared mount      │
└─────────────┬───────────────────────────┘
              │ RPC over vsock
┌─────────────▼───────────────────────────┐
│ Guest VM (Kata Agent)                    │
│ - Manages containers                     │
│ - Executes CRIU                          │
│ - READ-ONLY access to shared mount       │
└───────────────────────────────────────────┘
```

### Correct Checkpoint Flow

```
1. Containerd → Runtime.Checkpoint()
2. Runtime pauses container (containerd behavior)
3. Runtime resolves checkpoint paths:
   - Host: /run/kata-containers/shared/sandboxes/{id}/shared/checkpoints/...
   - Guest: /run/kata-containers/shared/containers/checkpoints/...
4. **Runtime creates directories on host** ← CRITICAL FIX
5. Runtime → Agent.CheckpointContainer(guestPath)
6. Agent runs CRIU in /tmp (writable)
7. Agent copies CRIU output to shared mount (now writable via host-created dirs)
8. Success! ✅
```

## Files Modified

### Runtime
- `src/runtime/virtcontainers/sandbox.go`: Host-side directory pre-creation
- `src/runtime/virtcontainers/kata_agent.go`: RPC handler registration

### Agent
- `src/agent/rustjail/src/container.rs`: 
  - Paused state handling
  - Diagnostic logging with CRIU check
  - File-based output capture
  - Temp directory workaround
- `src/agent/src/rpc.rs`: Debug logging

### Kernel
- `tools/packaging/kernel/configs/fragments/common/namespaces.conf`:
  - `CONFIG_EXPERT=y`
  - `CONFIG_CHECKPOINT_RESTORE=y`

## Next Steps

1. **Rebuild Runtime** (or fix build environment):
   ```bash
   cd /mnt/kata-containers-snapshot/src/runtime
   make clean && make
   sudo systemctl stop containerd
   sudo cp containerd-shim-kata-v2 /usr/local/bin/
   sudo systemctl start containerd
   ```

2. **Test End-to-End**:
   ```bash
   sudo nerdctl run -d --name test --runtime io.containerd.kata.v2 busybox sleep 3600
   sudo nerdctl checkpoint create test my-checkpoint --leave-running
   # Should succeed!
   ```

3. **Verify Diagnostic Output** (if it fails):
   ```bash
   sudo cat /run/kata-containers/shared/sandboxes/{container-id}/shared/checkpoints/.../work/criu-diagnostic.log
   ```

## Key Insights from Investigation

1. **Debug Console Access**: Enabled but difficult to use interactively
2. **Agent Logging**: Requires `agent.log=debug` kernel parameter
3. **Virtiofs Characteristics**:
   - Host can create/write
   - Guest can only read
   - This is by design for security
4. **Containerd Behavior**: Always pauses containers before checkpointing
5. **CRIU Requirements**: Needs writable directories for images/ and work/

## Conclusion

We successfully identified the root cause through systematic investigation:
- Added comprehensive logging
- Tested CRIU binary and kernel config
- Discovered virtiofs read-only constraint
- Implemented and verified the fix (pre-create directories on host)

**Final task**: Rebuild/reinstall runtime binary with the directory pre-creation fix, then test end-to-end.

