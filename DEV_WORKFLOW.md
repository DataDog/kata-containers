# Development Workflow - Kata Containers Checkpoint

This document describes the workflow for developing, building, and testing the Kata Containers checkpoint/restore functionality.

## Remote Machine Connection

**SSH Access:**
```bash
ssh -o StrictHostKeyChecking=no ubuntu@3.237.232.92
```

**Machine Details:**
- Host: `3.237.232.92`
- User: `ubuntu`
- Workspace: `/mnt/kata-containers-snapshot`
- Local workspace: `/Users/eric.mountain/go/src/github.com/DataDog/kata-containers-snapshot`

## File Synchronization

### Sync Agent Code to Remote
```bash
cd /Users/eric.mountain/go/src/github.com/DataDog/kata-containers-snapshot
rsync -avz src/agent/rustjail/src/container.rs \
  ubuntu@3.237.232.92:/mnt/kata-containers-snapshot/src/agent/rustjail/src/container.rs
```

### Sync Multiple Files
```bash
rsync -avz src/agent/ ubuntu@3.237.232.92:/mnt/kata-containers-snapshot/src/agent/
```

## Build Process

### Build Agent (Rust)
```bash
ssh -o StrictHostKeyChecking=no ubuntu@3.237.232.92 \
  'cd /mnt/kata-containers-snapshot/src/agent && ~/.cargo/bin/cargo build --release 2>&1 | tail -5'
```

**Output location:** `/mnt/kata-containers-snapshot/src/agent/target/release/kata-agent`

### Build Runtime (Go)
```bash
ssh -o StrictHostKeyChecking=no ubuntu@3.237.232.92 \
  'cd /mnt/kata-containers-snapshot/src/runtime && make'
```

## Install Agent into Guest Image

**Guest image path:**
```
/usr/share/kata-containers/kata-containers-2025-12-03-10:10:39.555800518+0000-d9782598e
```

**Install command:**
```bash
ssh -o StrictHostKeyChecking=no ubuntu@3.237.232.92 '
IMG=/usr/share/kata-containers/kata-containers-2025-12-03-10:10:39.555800518+0000-d9782598e
sudo guestfish -a $IMG -i \
  upload /mnt/kata-containers-snapshot/src/agent/target/release/kata-agent /usr/bin/kata-agent : \
  chmod 0755 /usr/bin/kata-agent
'
```

## Restart Services

After updating the agent or runtime, restart containerd:

```bash
ssh -o StrictHostKeyChecking=no ubuntu@3.237.232.92 \
  'sudo systemctl restart containerd && sleep 8 && echo "✅ Services restarted"'
```

## Testing

### Basic Checkpoint Test

```bash
ssh -o StrictHostKeyChecking=no ubuntu@3.237.232.92 '
# Clean up old containers
sudo nerdctl rm -f $(sudo nerdctl ps -aq) 2>/dev/null

# Create test container
sudo nerdctl run -d --name test-checkpoint \
  --runtime io.containerd.kata.v2 \
  busybox sleep 3600

# Wait for container to be ready
sleep 6

# Create checkpoint
sudo nerdctl checkpoint create test-checkpoint TEST-CHECKPOINT
'
```

### Test with Timing

```bash
ssh -o StrictHostKeyChecking=no ubuntu@3.237.232.92 '
sudo nerdctl rm -f $(sudo nerdctl ps -aq) 2>/dev/null
sudo nerdctl run -d --name timer-test --runtime io.containerd.kata.v2 busybox sleep 3600
sleep 6
echo "=== Starting checkpoint ==="
time sudo nerdctl checkpoint create timer-test TIMER-TEST
'
```

### Monitor CRIU Progress in Guest

Get the container full ID and exec into the guest:

```bash
ssh -o StrictHostKeyChecking=no ubuntu@3.237.232.92 '
FULL_ID=$(sudo nerdctl ps -a --no-trunc --filter name=test-checkpoint --format "{{.ID}}")
echo "Container ID: $FULL_ID"

# Check CRIU log
echo "tail -30 /tmp/criu-checkpoint-*/work/criu-dump.log; exit" | \
  sudo kata-runtime exec $FULL_ID /bin/sh

# Check CRIU process tree
echo "ps auxf | grep -E \"kata-agent|criu|sleep\" | grep -v grep; exit" | \
  sudo kata-runtime exec $FULL_ID /bin/sh
'
```

### Test Checkpoint Async with Monitoring

```bash
ssh -o StrictHostKeyChecking=no ubuntu@3.237.232.92 '
sudo nerdctl rm -f $(sudo nerdctl ps -aq) 2>/dev/null
sudo nerdctl run -d --name async-test --runtime io.containerd.kata.v2 busybox sleep 3600
sleep 6

# Start checkpoint in background
(timeout 60 sudo nerdctl checkpoint create async-test ASYNC-TEST 2>&1 >/tmp/chkpt.log &)

# Wait a moment for CRIU to start
sleep 3

FULL_ID=$(sudo nerdctl ps -a --no-trunc --filter name=async-test --format "{{.ID}}")
echo "Container: $FULL_ID"
echo ""
echo "=== Checking CRIU status ==="
echo "ps aux | grep criu | grep -v grep && echo && tail -10 /tmp/criu-checkpoint-*/work/criu-dump.log; exit" | \
  sudo kata-runtime exec $FULL_ID /bin/sh
'
```

## Manual CRIU Test (Baseline)

This is the **working** manual CRIU command from inside the guest VM:

```bash
ssh -o StrictHostKeyChecking=no ubuntu@3.237.232.92 '
sudo nerdctl run -d --name manual-test --runtime io.containerd.kata.v2 busybox sleep 3600
sleep 6
FULL_ID=$(sudo nerdctl ps -a --no-trunc --filter name=manual-test --format "{{.ID}}")

echo "cd /tmp && rm -rf manual-test && mkdir -p manual-test/images manual-test/work && \
/usr/sbin/criu dump --tree \$(pgrep -f \"sleep 3600\") \
  --images-dir /tmp/manual-test/images \
  --work-dir /tmp/manual-test/work \
  --log-file /tmp/manual-test/work/dump.log \
  --leave-running --file-locks \
  --skip-mnt /dev/shm \
  --skip-mnt /etc/hostname \
  --skip-mnt /etc/hosts \
  --skip-mnt /etc/resolv.conf && \
echo SUCCESS && ls -lh /tmp/manual-test/images/ | head -10; exit" | \
  sudo kata-runtime exec $FULL_ID /bin/sh
'
```

**Expected output:** `SUCCESS` and a list of checkpoint image files.

## Common Combined Workflow

**Sync, build, install, restart, test:**

```bash
cd /Users/eric.mountain/go/src/github.com/DataDog/kata-containers-snapshot && \
rsync -avz src/agent/rustjail/src/container.rs ubuntu@3.237.232.92:/mnt/kata-containers-snapshot/src/agent/rustjail/src/container.rs && \
ssh -o StrictHostKeyChecking=no ubuntu@3.237.232.92 '
  cd /mnt/kata-containers-snapshot/src/agent && \
  ~/.cargo/bin/cargo build --release 2>&1 | tail -3 && \
  IMG=/usr/share/kata-containers/kata-containers-2025-12-03-10:10:39.555800518+0000-d9782598e && \
  sudo guestfish -a $IMG -i upload target/release/kata-agent /usr/bin/kata-agent : chmod 0755 /usr/bin/kata-agent && \
  sudo systemctl restart containerd && sleep 8 && \
  echo "✅ Build complete, testing..." && \
  sudo nerdctl rm -f $(sudo nerdctl ps -aq) 2>/dev/null && \
  sudo nerdctl run -d --name quick-test --runtime io.containerd.kata.v2 busybox sleep 3600 && \
  sleep 6 && \
  time sudo nerdctl checkpoint create quick-test QUICK-TEST
'
```

## Debugging

### Check Agent Logs

```bash
ssh -o StrictHostKeyChecking=no ubuntu@3.237.232.92 \
  'sudo journalctl -u containerd --since "5 minutes ago" --no-pager | grep -E "CRIU|checkpoint" | tail -30'
```

### Check CRIU in Guest

```bash
ssh -o StrictHostKeyChecking=no ubuntu@3.237.232.92 '
FULL_ID=$(sudo nerdctl ps -a --no-trunc --filter name=test --format "{{.ID}}")
echo "journalctl -u kata-agent --since \"5 minutes ago\" -n 50 --no-pager; exit" | \
  sudo kata-runtime exec $FULL_ID /bin/sh
'
```

### Check Process Tree in Guest

```bash
echo "ps auxf; exit" | sudo kata-runtime exec <CONTAINER_ID> /bin/sh
```

### Check CRIU Parent Process ID

```bash
echo "cat /proc/\$(pgrep -f 'criu dump')/status | grep PPid; exit" | \
  sudo kata-runtime exec <CONTAINER_ID> /bin/sh
```

## Current Status

### ✅ Working
- Manual CRIU execution from guest shell completes successfully
- Agent spawns CRIU with proper parent hierarchy (sh -> criu)
- `--leave-running` flag is included
- All required kernel configs are enabled
- CRIU 3.19 is installed in guest

### ❌ Not Working  
- Agent-spawned CRIU hangs indefinitely
- Hangs at "Parasite syscall_ip" in CRIU log (line 389)
- Timeout after 3 minutes (180 seconds)

### Key Mystery
**Manual CRIU:** Completes in ~1 second  
**Agent-spawned CRIU:** Hangs forever at the same point

**Process trees:**
- **Manual:** `kata-runtime exec` → `sh` → `criu` (works)
- **Agent:** `kata-agent` → `sh` → `criu` (hangs)

Despite having the correct process hierarchy (not siblings), agent-spawned CRIU still hangs.

## Next Steps to Investigate

1. Compare environment variables between manual and agent-spawned CRIU
2. Check file descriptor differences
3. Try running CRIU with strace under both scenarios
4. Investigate if there's a signal mask or capability difference
5. Check if the tokio async runtime is interfering with CRIU's ptrace operations

