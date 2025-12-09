# CRIU wait4() Hang - Root Cause Analysis

## Problem
CRIU hangs indefinitely when called by the kata-agent, always stopping at "Parasite syscall_ip" with no further progress.

## Investigation

### Observations
1. **Manual CRIU succeeds**: When run from a shell via `kata-runtime exec`, CRIU completes successfully
2. **Agent-spawned CRIU hangs**: When spawned by kata-agent via Rust `Command::new().spawn()`, CRIU hangs
3. **strace shows wait4()**: Previous strace indicated CRIU was blocked in `wait4(126)` or similar

### Process Tree Analysis
```
root         118  kata-agent
root         125  \_ sleep 3600        (the container process)
root         144  \_ /usr/sbin/criu    (CRIU trying to checkpoint PID 125)
```

## Root Cause

**CRIU cannot `wait4()` on a sibling process.**

When CRIU (PID 144) is a direct child of kata-agent (PID 118), and the target process (PID 125) is ALSO a child of kata-agent, they are **siblings**. 

The `wait4()` system call can only wait on:
1. Direct children of the calling process
2. Processes in the same process group (with certain flags)

CRIU (PID 144) trying to `wait4(125)` fails because PID 125 is not its child - it's its sibling.

### Why Manual Test Worked
When we ran CRIU manually via `kata-runtime exec`:
```
root         118  kata-agent
root         125  \_ sleep 3600
root         195  \_ /bin/sh           (from kata-runtime exec)
root         201      \_ /usr/sbin/criu (CRIU as grandchild of kata-agent)
```

In this case, CRIU is NOT a sibling of the sleep process - they're in different branches of the tree. CRIU uses `ptrace` to attach to the target, which works across different process tree branches.

## Why The Current Approach Fails

The kata-agent spawns CRIU directly:
```rust
let mut child = Command::new("sh")
    .arg("-c")
    .arg(format!("exec {}", criu_cmd))
    .spawn()?;
```

Even with `exec` via sh, the resulting CRIU process is still a direct child of kata-agent, making it a sibling of the container process.

## Solutions

### Option 1: Double-fork
Make CRIU a grandchild by doing a double-fork:
```rust
Command::new("sh")
    .arg("-c")
    .arg("sh -c 'criu dump ...' &")
    .spawn()?;
```

### Option 2: Use an intermediate helper
Create a small helper process that spawns CRIU and exits, orphaning CRIU to init.

### Option 3: Change process relationships
This might require deeper changes to how rustjail manages processes.

### Option 4: Don't use wait4()
Modify how we wait for CRIU - instead of calling `.wait()` on the child, poll `/proc/{pid}` until it disappears. **WE'RE ALREADY DOING THIS** but the problem is CRIU itself is hung on its internal wait4() call.

## The Real Issue

Even though our Rust code polls `/proc` instead of calling `.wait()`, CRIU itself internally does `wait4()` as part of its checkpoint operation. When CRIU's internal `wait4()` blocks because it's waiting on a sibling, our polling doesn't help.

##Next Step

Try spawning CRIU in a way that breaks the sibling relationship - perhaps via `setsid` + background, or a helper script.

