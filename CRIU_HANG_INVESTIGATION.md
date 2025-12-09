# CRIU Hang Investigation

**Date:** December 5, 2025  
**Status:** CRIU hangs indefinitely during checkpoint operation

## Symptoms

1. **CRIU Process State:** 
   - Process running (PID 161 in guest)
   - State: `S` (sleeping/waiting)
   - No CPU usage
   - Has been running for 5+ minutes

2. **File Descriptors:**
   ```
   FD 14 -> /tmp/criu-checkpoint-.../images/files.img (write)
   ```
   - CRIU has `files.img` open for writing
   - File size: 0 bytes (nothing written yet)

3. **Log Files:**
   - `criu-stdout.log`: 0 bytes (empty)
   - `criu-stderr.log`: 0 bytes (empty)
   - `criu-dump.log`: 0 bytes (empty)
   - **No output at all from CRIU**

4. **Files Created:**
   ```
   images/files.img      (0 bytes)
   images/seccomp.img    (0 bytes)
   work/criu-dump.log    (0 bytes)
   ```

## CRIU Command
```bash
/usr/sbin/criu dump \
  --tree 126 \
  --images-dir /tmp/criu-checkpoint-*/images \
  --work-dir /tmp/criu-checkpoint-*/work \
  --log-file /tmp/criu-checkpoint-*/work/criu-dump.log \
  --tcp-established \
  --ext-unix-sk \
  --shell-job \
  --file-locks \
  --skip-mnt /dev/shm \
  --skip-mnt /etc/hostname \
  --skip-mnt /etc/hosts \
  --skip-mnt /etc/resolv.conf \
  --leave-running
```

## Kernel Configuration (All Enabled ✅)
- `CONFIG_CHECKPOINT_RESTORE=y`
- `CONFIG_EXPERT=y`
- `CONFIG_PROC_PAGE_MONITOR=y`
- `CONFIG_MEMBARRIER=y`
- `CONFIG_MEM_SOFT_DIRTY=y`
- `CONFIG_USERFAULTFD=y`
- `CONFIG_RSEQ=y`
- `CONFIG_TIME_NS=y`

## Possible Causes

### 1. **Ptrace/Process Tracing Issue**
CRIU uses ptrace to freeze and inspect processes. The hang might be in:
- Initial ptrace attach
- Process tree traversal
- Memory region scanning

**Evidence:** Process is in 'S' state (sleeping), suggesting it's waiting on a syscall

### 2. **File I/O Blocking**
CRIU has `files.img` open but hasn't written anything.
- Could be blocking on first write to /tmp filesystem
- Might be a tmpfs issue in the guest VM
- Could be waiting for some file descriptor operation

### 3. **Missing Kernel Feature**
Despite having the main configs, CRIU might need:
- `CONFIG_IA32_EMULATION` (for 32-bit support)
- `CONFIG_COMPAT` (for compatibility layer)
- `CONFIG_FHANDLE` (for file handle operations)
- `CONFIG_EVENTFD` (for event notifications)
- `CONFIG_EPOLL` (for I/O event notifications)
- `CONFIG_INOTIFY_USER` (for file monitoring)

### 4. **--leave-running Flag**
The `--leave-running` flag tells CRIU to not kill the process after checkpoint.
- This might require additional kernel features
- Could be waiting for some condition that never occurs
- Might be incompatible with our container setup

### 5. **Systemd Init Process**
The guest uses systemd as PID 1, which might interfere:
- systemd might be holding locks CRIU needs
- cgroups managed by systemd might conflict
- systemd's journal or other services might block CRIU

### 6. **Network/Socket Handling**
The `--tcp-established` and `--ext-unix-sk` flags handle network connections:
- Might be stuck scanning network namespaces  
- Could be waiting on socket operations
- External unix sockets might have issues

## Next Steps

### Immediate Actions:
1. **Add verbose logging to CRIU:**
   - Use `-v4` or `--verbosity 4` flag
   - This will show exactly where CRIU is stuck

2. **Try without --leave-running:**
   - Remove this flag to see if it's the cause
   - Test with process termination

3. **Simplify CRIU flags:**
   - Remove `--tcp-established`
   - Remove `--ext-unix-sk`
   - Remove `--shell-job`
   - Try minimal checkpoint first

4. **Check additional kernel configs:**
   ```
   CONFIG_FHANDLE
   CONFIG_EVENTFD
   CONFIG_EPOLL
   CONFIG_INOTIFY_USER
   CONFIG_UNIX
   CONFIG_INET
   CONFIG_PACKET
   ```

5. **Install strace in guest:**
   - Get static strace binary
   - Wrap CRIU execution with strace
   - See exact syscall where it hangs

### Debugging Strategy:
```rust
// Modify container.rs to add verbose logging
cmd.arg("dump")
    .arg("-v4")  // <-- ADD THIS
    .arg("--tree")
    .arg(self.init_process_pid.to_string())
    // ... rest of args
```

## Files Modified So Far

### Runtime (Timeout Fix)
- `src/runtime/virtcontainers/kata_agent.go` - Added 10-minute timeout for checkpoint requests

### Agent (Session Leader Fix)
- `src/agent/rustjail/src/container.rs` - Unconditional setsid() for init processes

### Kernel (CRIU Configs)
- `tools/packaging/kernel/configs/fragments/common/namespaces.conf` - All CRIU-required configs

## Current Blockers

1. **Cannot determine where CRIU is stuck** - need strace or verbose logging
2. **All logs are empty** - CRIU hasn't started writing output
3. **Process is sleeping** - waiting on some kernel operation

## Hypothesis

**Most Likely:** CRIU is blocked in an early initialization phase, possibly:
- Opening /proc files
- Attaching ptrace to target process
- Scanning process memory maps
- Initializing internal data structures

The fact that NO output has been written suggests it's failing very early, before CRIU's logging system is fully initialized.

