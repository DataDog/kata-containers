# Kata Containers Checkpoint/Restore - Implementation Progress

**Date**: December 5, 2025  
**Status**: Infrastructure Fixed ✅ - CRIU Investigation Ongoing

## Progress Summary

### ✅ COMPLETED - Infrastructure Fixed!

1. **Root Cause Identified**: Virtiofs mount architecture
   - `/run/kata-containers/shared/sandboxes/{id}/mounts/` is writable from host
   - `/run/kata-containers/shared/sandboxes/{id}/shared/` is read-only bind mount of `mounts/`
   - Agent sees `shared/` as read-only via virtiofs

2. **Solution Implemented**: 
   ```go
   // Changed checkpoint.go to use mounts/ instead of shared/
   func (s *Sandbox) checkpointHostBase() string {
       return filepath.Join(getMountPath(s.id), checkpointDirName)  // was GetSharePath
   }
   ```

3. **Directory Pre-Creation**: Runtime now creates directories on host before calling agent
   ```go
   imagesDir := filepath.Join(hostDir, "images")
   workDir := filepath.Join(hostDir, "work")
   os.MkdirAll(imagesDir, 0755)
   os.MkdirAll(workDir, 0755)
   ```

4. **Verification**: 
   - Directories successfully created in `mounts/checkpoints/`
   - Visible in guest as `/run/kata-containers/shared/containers/checkpoints/` (via bind mount)
   - Runtime logs confirm: "DEBUG: Checkpoint directories created and resolved"

### ⚠️ ONGOING - CRIU Execution Issue

**Current Problem**:
- CRIU executes but returns exit code 1
- stdout and stderr are both empty
- No diagnostic file created (agent fails before copy)

**Evidence**:
```
STDOUT (/tmp/criu-checkpoint-.../criu-stdout.log): .
STDERR (/tmp/criu-checkpoint-.../criu-stderr.log): .
```

**Possible Causes**:
1. CRIU segfaulting immediately (no output)
2. Missing kernel feature CRIU requires
3. Agent's Command::output() not capturing output properly in VM context
4. CRIU permissions/capabilities issue

## Technical Architecture

### Directory Structure (Fixed)
```
Host:
/run/kata-containers/shared/sandboxes/{id}/
├── mounts/          ← WRITABLE, used for file staging
│   └── checkpoints/
│       └── {container_id}/
│           └── {checkpoint_id}/
│               ├── images/
│               └── work/
└── shared/          ← READ-ONLY bind mount of mounts/
    └── checkpoints/ ← visible to guest via virtiofs
```

### Checkpoint Flow (Working)
1. ✅ Containerd calls runtime.Checkpoint()
2. ✅ Runtime creates directories in mounts/
3. ✅ Runtime calls agent with guest paths
4. ⚠️ Agent runs CRIU → **FAILS HERE**
5. ❌ Agent would copy results to shared mount

## Files Modified

### Runtime (Fixed)
- `src/runtime/virtcontainers/checkpoint.go`: Use `getMountPath()` instead of `GetSharePath()`
- `src/runtime/virtcontainers/sandbox.go`: Pre-create directories in `mounts/`

### Agent (Has diagnostics)
- `src/agent/rustjail/src/container.rs`: 
  - CRIU check before dump
  - File-based output capture
  - Diagnostic logging
  - Temp directory workaround

### Kernel (Fixed)
- `tools/packaging/kernel/configs/fragments/common/namespaces.conf`: Added `CONFIG_CHECKPOINT_RESTORE=y`

## Next Steps for Investigation

1. **Test CRIU directly in guest** (via debug console or exec)
   ```bash
   kata-runtime exec {container_id}
   /usr/sbin/criu check
   /usr/sbin/criu dump --tree <pid> --images-dir /tmp/test
   ```

2. **Check for missing kernel features**
   ```bash
   # In guest
   /usr/sbin/criu check --all
   ```

3. **Verify CRIU can write output files**
   ```bash
   # Test if basic file operations work
   /usr/sbin/criu --help > /tmp/test.txt
   ```

4. **Consider alternative output capture**
   - Try synchronous execution instead of tokio::Command
   - Redirect directly to files instead of pipes
   - Add strace to see syscall failures

## Success Criteria

- [x] Directories created successfully on host
- [x] Directories accessible from guest
- [ ] CRIU executes without errors
- [ ] CRIU creates checkpoint images
- [ ] Images copied to shared directory
- [ ] Container can be restored

## Key Learnings

1. **Virtiofs Architecture**: The `shared/` directory is intentionally read-only from guest for security
2. **Kata Design**: Use `mounts/` for writable staging, `shared/` is the VM view via bind mount  
3. **Debugging**: Added comprehensive logging at every layer (runtime, agent, CRIU)
4. **Infrastructure vs Application**: Fixed infrastructure (directory creation), now debugging application (CRIU execution)

## Conclusion

**Major milestone achieved!** We've successfully resolved the infrastructure issue that was preventing checkpoint directory creation. The directories are now being created correctly on the host in `mounts/` and are accessible to the guest via the `shared/` bind mount.

The remaining issue is with CRIU execution itself - it's failing immediately with no output. This suggests either:
- A CRIU-specific problem (missing feature, permission issue)
- An execution environment issue (how Command::output() works in the VM)

This is a much smaller, more focused problem to solve compared to the infrastructure issue we just fixed.

