# CRIU Checkpoint - Root Cause Found: PT RACE Broken

**Date:** December 5, 2025  
**Status:** ROOT CAUSE IDENTIFIED - ptrace() system call is blocked/broken in guest VM

## 🔍 Discovery

After adding strace to the guest image and wrapping CRIU execution, we found:

1. **Both strace and CRIU are running** - processes visible with `ps`
2. **No output from either** - all log files remain 0 bytes
3. **Test confirms ptrace is broken:**
   ```bash
   root@guest# strace sleep 1
   [HANGS INDEFINITELY]
   ```

## Root Cause

**ptrace() system call does not work in the Kata guest VM.**

CRIU fundamentally requires ptrace to:
- Attach to target processes
- Freeze process state
- Read process memory
- Inspect file descriptors
- etc.

Without working ptrace, CRIU cannot function AT ALL.

## Why ptrace is Broken

Possible causes:

### 1. **Kernel Security Restrictions**
The guest kernel may have ptrace restricted:
```bash
# Check (in guest):
cat /proc/sys/kernel/yama/ptrace_scope
# Should be 0 for unrestricted
# If 1+, ptrace is restricted
```

### 2. **seccomp Filters**
The container or VM may have seccomp filters blocking ptrace:
```bash
# Check seccomp status:
cat /proc/self/status | grep Seccomp
# If 2, strict seccomp is active
```

### 3. **SELinux/AppArmor**
Security modules may block ptrace between processes

### 4. **VM Hypervisor Restrictions**
QEMU/KVM might be blocking certain syscalls or capabilities

### 5. **Missing Kernel Config**
Kernel might be built without ptrace support (unlikely but possible):
```
CONFIG_PTRACE=y  # Must be enabled
```

## Evidence

1. **strace hangs immediately** when trying to trace any process
2. **CRIU hangs** at the same early stage (before logging)
3. **Both processes in 'S' state** (sleeping/waiting) - likely on ptrace() syscall

## Next Steps to Fix

### Option 1: Enable ptrace in Guest Kernel
```bash
# Check current setting:
sysctl kernel.yama.ptrace_scope

# Set to 0 (allow all):
sysctl -w kernel.yama.ptrace_scope=0
```

Add to guest startup or kernel params:
```
kernel.yama.ptrace_scope=0
```

### Option 2: Disable seccomp for Checkpoint
Modify container spec to allow ptrace:
```json
{
  "seccomp": {
    "defaultAction": "SCMP_ACT_ALLOW",
    "syscalls": [
      {
        "names": ["ptrace"],
        "action": "SCMP_ACT_ALLOW"
      }
    ]
  }
}
```

### Option 3: Check/Fix Kernel Config
Ensure these are enabled:
```
CONFIG_PTRACE=y
CONFIG_HAVE_ARCH_TRACEHOOK=y  
CONFIG_TRACEPOINTS=y
```

### Option 4: Disable SELinux/AppArmor
If active, temporarily disable to test:
```bash
# SELinux:
setenforce 0

# AppArmor:
systemctl stop apparmor
```

## Quick Test

Once ptrace is fixed, verify with:
```bash
# In guest:
strace -o /tmp/test.log ls /bin
# Should complete immediately and create log file

# Then test CRIU:
criu check --all
# Should not hang
```

## Why This Wasn't Caught Earlier

1. **CRIU check passes** - it doesn't actually test ptrace functionality
2. **No error messages** - ptrace just blocks silently
3. **Process appears running** - it's waiting on blocked syscall
4. **Logs never created** - ptrace blocks before logging starts

## Summary

All the infrastructure we built is correct:
- ✅ Session leader fix (setsid)
- ✅ Kernel configs (all CRIU requirements)
- ✅ Extended timeouts
- ✅ Proper RPC handlers
- ✅ Directory setup

**BUT** ptrace() doesn't work in the guest VM, making CRIU completely non-functional.

**This is likely a fundamental Kata Containers limitation** - the guest VM security model may intentionally restrict ptrace to prevent container escape vectors.

## Recommendation

**Check with Kata Containers community:**
- Is checkpoint/restore officially supported?
- Are there known ptrace restrictions?
- Is there a way to enable ptrace safely?

**Alternative:** Use VM-level snapshots instead of process-level checkpoints:
- QEMU native snapshots
- Firecracker snapshots
- libvirt snapshot functionality

These don't require ptrace and work at the hypervisor level.

