# Fix Build Issues

## Problem

When running `anchor build` or `cargo build`, you may encounter:
1. `anchor build` does nothing and hangs
2. `cargo build` shows "Blocking waiting for file lock on package cache"

## Root Cause

Multiple stuck processes from previous build attempts are holding locks on:
- Cargo package cache
- Build processes that didn't terminate properly

## Solution

### Step 1: Kill All Stuck Processes

```bash
# Kill all anchor and cargo-build-bpf processes
pkill -9 -f "anchor build"
pkill -9 -f "cargo-build-bpf"
pkill -9 -f "cargo.*bpfel"

# Verify they're gone
ps aux | grep -E "(anchor|cargo-build-bpf)" | grep -v grep
```

### Step 2: Remove Package Cache Lock

```bash
# Remove the lock file
rm -f ~/.cargo/.package-cache

# Wait a moment
sleep 1
```

### Step 3: Try Building Again

```bash
cd program
anchor build
```

## Prevention

If builds get stuck again:

1. **Don't run multiple builds at once** - wait for one to finish
2. **If a build hangs, kill it properly:**
   ```bash
   # Find the process
   ps aux | grep "anchor build"
   
   # Kill it
   kill -9 <PID>
   ```
3. **Check for stuck processes before starting a new build:**
   ```bash
   ps aux | grep -E "(anchor|cargo)" | grep -v grep
   ```

## Alternative: Build in Background

If you want to build in the background:

```bash
cd program
nohup anchor build > build.log 2>&1 &

# Check progress
tail -f build.log
```

## Quick Fix Script

```bash
#!/bin/bash
# Kill stuck processes
pkill -9 -f "anchor build"
pkill -9 -f "cargo-build-bpf"
pkill -9 -f "cargo.*bpfel"

# Remove locks
rm -f ~/.cargo/.package-cache

# Wait
sleep 2

echo "Cleanup complete. You can now run 'anchor build'"
```

