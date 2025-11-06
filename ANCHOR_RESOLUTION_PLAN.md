# Safe Resolution Plan for Anchor Version Manager (AVM) Conflict

## Problem Summary

The conflict occurs when `avm install` attempts to install Anchor via `cargo install`, but cargo refuses to overwrite an existing `anchor` binary in `~/.cargo/bin/anchor`. This happens because:

1. A direct `cargo install` of `anchor-cli` creates a binary at `~/.cargo/bin/anchor`
2. `avm` manages versions in `~/.avm/bin/` as `anchor-<version>` binaries and should create a symlink from `~/.cargo/bin/anchor` to the active version's binary
3. When `avm install` tries to use `cargo install`, it conflicts with the existing binary/symlink
4. Additionally, `avm use` may not create the symlink automatically, requiring manual creation

## Current State

- ✅ Anchor 0.22.1 is currently installed and active (via manual workaround)
- ✅ **RESOLVED**: Symlink created successfully at `~/.cargo/bin/anchor -> ~/.avm/bin/anchor-0.22.1`
- ✅ `anchor --version` now works correctly showing `anchor-cli 0.22.1`
- ⚠️ The installation was done manually, bypassing `avm install`
- ⚠️ Future `avm install` commands may still fail with the same conflict
- ⚠️ **NOTE**: `avm use <version>` may not create the symlink automatically - use `fix-avm-symlink.sh` if needed

## Recommended Safe Resolution Plan

### Phase 1: Assessment (No Changes)

1. **Verify current state:**
   ```bash
   anchor --version
   avm list
   ls -la ~/.cargo/bin/anchor
   ls -la ~/.avm/bin/
   ```

2. **Check for any cargo-installed anchor packages:**
   ```bash
   cargo install --list | grep -i anchor
   ```

3. **Document current PATH configuration:**
   ```bash
   echo $PATH | tr ':' '\n' | grep -E "(cargo|avm)"
   ```

### Phase 2: Clean Separation (Recommended Approach)

**Option A: Full AVM Management (Recommended)**

1. **Remove any directly cargo-installed anchor binaries:**
   ```bash
   # Check what's installed
   cargo install --list | grep anchor
   
   # If anchor-cli is listed, uninstall it
   cargo uninstall anchor-cli
   
   # Remove any direct binary if it exists
   rm -f ~/.cargo/bin/anchor
   ```

2. **Ensure AVM manages the symlink:**
   ```bash
   # Verify avm can create the symlink
   avm use 0.22.1
   
   # Check the symlink is correct
   ls -la ~/.cargo/bin/anchor
   # Should show: anchor -> /Users/jal/.avm/bin/anchor-0.22.1
   
   # If symlink doesn't exist, create it manually:
   VERSION=$(cat ~/.avm/.version)
   ln -sf ~/.avm/bin/anchor-$VERSION ~/.cargo/bin/anchor
   ```

3. **Test installation of a new version:**
   ```bash
   # Try installing a different version to verify avm works
   avm install 0.23.0
   avm use 0.23.0
   anchor --version
   
   # Switch back
   avm use 0.22.1
   ```

**Option B: Hybrid Approach (If Option A Fails)**

If `avm install` continues to fail due to cargo conflicts:

1. **Keep using manual installation for problematic versions:**
   ```bash
   # For versions that fail with avm install, use manual build:
   git clone --depth 1 --branch v0.22.1 https://github.com/coral-xyz/anchor.git /tmp/anchor-0.22.1
   cd /tmp/anchor-0.22.1/cli
   cargo build --release
   cp target/release/anchor ~/.avm/bin/anchor-0.22.1
   chmod +x ~/.avm/bin/anchor-0.22.1
   avm use 0.22.1
   rm -rf /tmp/anchor-0.22.1
   ```

2. **Use `avm install` for versions that work normally**

### Phase 3: Prevention

1. **Always use `avm` for Anchor installations:**
   - Never use `cargo install anchor-cli` directly
   - Always use `avm install <version>` or manual build + copy to `~/.avm/bin/`

2. **Add to your shell profile (`~/.zshrc` or `~/.bashrc`):**
   ```bash
   # Ensure ~/.cargo/bin is in PATH (should already be there)
   export PATH="$HOME/.cargo/bin:$PATH"
   ```

3. **Document version requirements:**
   - Your project requires Anchor 0.22.1 (see `client/Cargo.toml`)
   - Keep this documented in project README

## Immediate Action Items

### Safe Steps (Can be done now):

1. ✅ **Verify current installation works:**
   ```bash
   anchor --version  # Should show 0.22.1
   ```

2. **Check if there are any cargo-installed packages:**
   ```bash
   cargo install --list | grep anchor
   ```

3. **Verify AVM structure:**
   ```bash
   ls -la ~/.avm/bin/
   avm list
   ```

### If Issues Persist:

1. **Fix missing symlink (Current Issue):**
   ```bash
   # Get the current version from AVM
   VERSION=$(cat ~/.avm/.version)
   
   # Ensure ~/.cargo/bin exists
   mkdir -p ~/.cargo/bin
   
   # Create symlink manually
   ln -sf ~/.avm/bin/anchor-$VERSION ~/.cargo/bin/anchor
   
   # Verify
   ls -la ~/.cargo/bin/anchor
   anchor --version
   ```

2. **Clean slate approach:**
   ```bash
   # Backup current state
   cp -r ~/.avm ~/.avm.backup
   
   # Remove conflicting binaries
   rm -f ~/.cargo/bin/anchor
   cargo uninstall anchor-cli 2>/dev/null || true
   
   # Reinstall via avm
   avm install 0.22.1
   avm use 0.22.1
   
   # If symlink still missing, create it manually (see step 1 above)
   ```

3. **If avm install still fails, use the manual method** (already done, but document it)

## Long-term Solution

The root cause is that `avm install` doesn't properly pass `--force` to `cargo install`. This is a known issue with AVM. Options:

1. **Wait for AVM fix** - The maintainers should fix `avm install` to handle existing binaries
2. **Use manual installation** - For now, use the manual build method for problematic versions
3. **Contribute fix** - If you're comfortable, you could contribute a fix to the AVM repository

## Verification Checklist

After resolution, verify:

- [x] `anchor --version` shows correct version (0.22.1) ✅ **VERIFIED**
- [x] `avm list` shows 0.22.1 as installed and current ✅ **VERIFIED**
- [x] `~/.cargo/bin/anchor` is a symlink to `~/.avm/bin/anchor-0.22.1` ✅ **VERIFIED**
- [x] `anchor` command is accessible in PATH ✅ **VERIFIED**
- [ ] Project builds successfully: `cd program && anchor build` (requires Solana CLI v1.9.13 - see SOLANA_SETUP.md)
- [ ] Can switch versions: `avm use 0.31.1 && anchor --version && avm use 0.22.1` (test when needed)

## Notes

- Your project specifically requires Anchor 0.22.1 (see `client/Cargo.toml` line 28)
- Your project requires Solana CLI v1.9.13 for `build-bpf` tool (see `program/README.md`)
- The manual installation method is safe and produces the same result
- AVM version switching should still work even with manually installed binaries
- Keep `~/.cargo/bin/` in your PATH (usually handled by cargo)
- **IMPORTANT**: If `avm use` doesn't create the symlink, you may need to create it manually:
  ```bash
  VERSION=$(cat ~/.avm/.version)
  ln -sf ~/.avm/bin/anchor-$VERSION ~/.cargo/bin/anchor
  ```

## Additional Setup Required

After fixing the Anchor installation, you may encounter:
- **`build-bpf` not found error**: This requires Solana CLI v1.9.13 installation
- See `SOLANA_SETUP.md` for detailed instructions on installing the correct Solana CLI version

## Quick Fix Script

A helper script `fix-avm-symlink.sh` has been created in the project root. Run it to automatically fix the symlink:

```bash
# Make sure it's executable
chmod +x fix-avm-symlink.sh

# Run it
./fix-avm-symlink.sh
```

Or run the commands manually:

```bash
# Get current version
VERSION=$(cat ~/.avm/.version)

# Ensure directory exists
mkdir -p ~/.cargo/bin

# Create symlink
ln -sf ~/.avm/bin/anchor-$VERSION ~/.cargo/bin/anchor

# Verify
ls -la ~/.cargo/bin/anchor
anchor --version
```

This script will:
1. Read the current AVM version from `~/.avm/.version`
2. Create the symlink from `~/.cargo/bin/anchor` to `~/.avm/bin/anchor-<version>`
3. Verify the symlink works

**Note**: If you see `avm` in `cargo install --list` pointing to `/private/tmp/anchor-0.22.1/avm`, this is from the temporary build we did earlier. It shouldn't interfere, but if you want to clean it up, you can uninstall it (though it may not be necessary).

