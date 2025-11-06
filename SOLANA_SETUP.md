# Solana CLI Setup for Anchor 0.22.1

## Problem

When running `anchor build`, you may encounter:
```
error: no such command: `build-bpf`
help: a command with a similar name exists: `build-sbf`
```

This happens because:
- Anchor 0.22.1 with Solana SDK 1.9.9 requires the older `build-bpf` tool
- Newer Solana CLI versions use `build-sbf` instead
- Your project specifically needs Solana CLI v1.9.13 (see `program/README.md`)

## Solution

Install Solana CLI v1.9.13:

```bash
sh -c "$(curl -sSfL https://release.solana.com/v1.9.13/install)"
```

**If you encounter SSL connection errors**, see `SOLANA_INSTALL_ALTERNATIVES.md` for alternative installation methods.

After installation, verify:

```bash
solana --version
# Should show: solana-cli 1.9.13

cargo install --list | grep build-bpf
# Should show: cargo-build-bpf v1.9.13
```

## Switching Between Versions

According to `program/README.md`:

- **For stable builds**: Use stable Solana CLI
  ```bash
  sh -c "$(curl -sSfL https://release.solana.com/stable/install)"
  ```

- **For mainnet forking**: Use v1.9.13
  ```bash
  sh -c "$(curl -sSfL https://release.solana.com/v1.9.13/install)"
  ```

## Verify Installation

After installing Solana CLI v1.9.13:

```bash
# Check Solana version
solana --version

# Check build-bpf is available
cargo build-bpf --version

# Try building the Anchor program
cd program
anchor build
```

## Notes

- The project uses Solana SDK 1.9.9 (see `client/Cargo.toml`)
- Anchor 0.22.1 is compatible with Solana CLI v1.9.13
- You may need to restart your terminal after installation for PATH changes to take effect

