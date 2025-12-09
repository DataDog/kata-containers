# CRIU Kernel Configuration Analysis for Kata Containers

## Summary

This document summarizes the analysis of CRIU (Checkpoint/Restore in Userspace) kernel configuration requirements for Kata Containers guest VMs.

**✅ STATUS: COMPLETED** - All CRIU kernel options have been added and the kernel has been rebuilt and installed.

## Current Status (Updated: Dec 4, 2025)

### Host Kernel (AWS 6.8.0-1043-aws)
✅ **PASSES** - `sudo criu check --all` reports: "Looks good."

All required CRIU kernel options are present:
- ✅ CONFIG_CHECKPOINT_RESTORE=y
- ✅ CONFIG_EXPERT=y
- ✅ CONFIG_EVENTFD=y
- ✅ CONFIG_FHANDLE=y
- ✅ CONFIG_EPOLL=y
- ✅ CONFIG_PACKET_DIAG=m (module)
- ✅ CONFIG_UNIX_DIAG=m (module)
- ✅ CONFIG_INET_DIAG=m (module)
- ✅ CONFIG_INET_UDP_DIAG=m (module)
- ✅ CONFIG_NETLINK_DIAG=m (module)
- ✅ CONFIG_NETFILTER_XT_MARK=m (module)
- ✅ CONFIG_TUN=y

### Kata Guest Kernel (vmlinux-6.12.47-174) ✅ UPDATED

**All CRIU Configuration Now Present:**
- ✅ CONFIG_CHECKPOINT_RESTORE=y
- ✅ CONFIG_EXPERT=y
- ✅ CONFIG_EVENTFD=y
- ✅ CONFIG_FHANDLE=y
- ✅ CONFIG_EPOLL=y
- ✅ CONFIG_PACKET_DIAG=y
- ✅ CONFIG_TUN=y
- ✅ CONFIG_NETFILTER_XT_MARK=y
- ✅ CONFIG_UNIX_DIAG=y (newly added)
- ✅ CONFIG_INET_DIAG=y (newly added)
- ✅ CONFIG_INET_UDP_DIAG=y (newly added)
- ✅ CONFIG_NETLINK_DIAG=y (newly added)

**Previous Version (vmlinux-6.12.47-173) - Missing:**
- ❌ CONFIG_UNIX_DIAG
- ❌ CONFIG_INET_DIAG
- ❌ CONFIG_INET_UDP_DIAG
- ❌ CONFIG_NETLINK_DIAG

## Actions Taken

### 1. Updated Kernel Configuration Fragments

Added missing CRIU-required options to the kernel configuration fragments:

**File: `tools/packaging/kernel/configs/fragments/common/fs.conf`**
```
+CONFIG_EVENTFD=y
```

**File: `tools/packaging/kernel/configs/fragments/common/network.conf`**
```
+CONFIG_UNIX_DIAG=y
+CONFIG_INET_DIAG=y
+CONFIG_INET_UDP_DIAG=y
+CONFIG_NETLINK_DIAG=y
```

### 2. Verification Method

Created test scripts to verify CRIU support:
- `test-criu-check.sh` - Automated script to test CRIU in kata guest VM
- `check-criu-kernel.sh` - Script to check kernel configuration for CRIU support

## Completed Actions

### 1. ✅ Updated Kernel Configuration Fragments
- Added missing CRIU options to `tools/packaging/kernel/configs/fragments/common/fs.conf`
- Added missing CRIU options to `tools/packaging/kernel/configs/fragments/common/network.conf`
- Incremented `kata_config_version` from 173 to 174

### 2. ✅ Rebuilt Kata Guest Kernel
```bash
cd /mnt/kata-containers-snapshot/tools/packaging/kernel
./build-kernel.sh -v 6.12.47 -f setup  # Force regenerate config with new options
./build-kernel.sh -v 6.12.47 build     # Build kernel (completed in ~40 seconds)
```

### 3. ✅ Installed New Kernel
```bash
sudo ./build-kernel.sh -v 6.12.47 install
# Installed to: /usr/share/kata-containers/vmlinux-6.12.47-174
# Updated symlink: vmlinux.container -> vmlinux-6.12.47-174
```

### 4. ✅ Verified CRIU Configuration
Created new Kata container with updated kernel and verified all CRIU options:
```bash
$ sudo ctr task exec --exec-id check criu-verify-new sh -c \
  "zcat /proc/config.gz | grep -E 'CONFIG_(CHECKPOINT_RESTORE|UNIX_DIAG|INET_DIAG|INET_UDP_DIAG|NETLINK_DIAG)='"

CONFIG_CHECKPOINT_RESTORE=y
CONFIG_INET_DIAG=y
CONFIG_INET_UDP_DIAG=y
CONFIG_NETLINK_DIAG=y
CONFIG_UNIX_DIAG=y
```

## Next Steps for CRIU Testing

Now that the kernel is properly configured, you can:

1. **Test CRIU functionality**:
   - Install CRIU in a kata container or add it to the guest image
   - Run `criu check --all` to verify all kernel features
   - Test actual checkpoint/restore operations with your application

2. **Example CRIU test**:
   ```bash
   # Create a container with a running process
   sudo ctr run --runtime io.containerd.kata.v2 -d ubuntu:latest test-app /bin/bash
   
   # Checkpoint the container (if CRIU is available in guest)
   # This requires additional setup beyond just the kernel config
   ```

## CRIU Kernel Requirements Reference

Based on [CRIU documentation](https://criu.org/Check_the_kernel) and analysis, the complete set of required kernel options:

### Core Features
- CONFIG_CHECKPOINT_RESTORE=y ✅
- CONFIG_EXPERT=y ✅

### Namespace Support
- CONFIG_NAMESPACES=y ✅
- CONFIG_UTS_NS=y ✅
- CONFIG_IPC_NS=y ✅
- CONFIG_PID_NS=y ✅
- CONFIG_NET_NS=y ✅
- CONFIG_USER_NS=y ✅

### File Handle and Event Support
- CONFIG_FHANDLE=y ✅
- CONFIG_EVENTFD=y ✅ (newly added)
- CONFIG_EPOLL=y ✅

### Socket Monitoring Interfaces
- CONFIG_UNIX_DIAG=y/m ⚠️ (newly added, needs rebuild)
- CONFIG_INET_DIAG=y/m ⚠️ (newly added, needs rebuild)
- CONFIG_INET_UDP_DIAG=y/m ⚠️ (newly added, needs rebuild)
- CONFIG_PACKET_DIAG=y/m ✅
- CONFIG_NETLINK_DIAG=y/m ⚠️ (newly added, needs rebuild)

### Network Support
- CONFIG_TUN=y ✅
- CONFIG_NETFILTER_XT_MARK=y/m ✅

## Notes

1. **Debug Console Configuration**: To use `kata-runtime exec` for guest access, add `agent.debug_console` to kernel parameters in `/etc/kata-containers/configuration.toml`:
   ```toml
   kernel_params = "... agent.debug_console"
   ```

2. **Module vs Built-in**: Options can be compiled as modules (=m) or built into the kernel (=y). Both work for CRIU.

3. **Verification Without CRIU Binary**: You can verify kernel support without installing CRIU by checking:
   ```bash
   # Check if CONFIG_CHECKPOINT_RESTORE is enabled
   ls -la /proc/sys/kernel/ns_last_pid
   
   # Check all CRIU configs
   zcat /proc/config.gz | grep -E 'CONFIG_(CHECKPOINT_RESTORE|UNIX_DIAG|INET_DIAG|...)'
   ```

## References

- [CRIU Kernel Requirements](https://criu.org/Check_the_kernel)
- [Kata Containers Developer Guide](docs/Developer-Guide.md)
- [Kernel Configuration Documentation](tools/packaging/kernel/README.md)

