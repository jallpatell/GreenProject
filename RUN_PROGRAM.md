# How to Run the Solana Arbitrage Bot

## Prerequisites

✅ **Completed:**
- Anchor 0.22.1 installed and working
- Solana CLI v1.9.13 installed with `build-bpf` tool
- All dependencies should be ready

## Step-by-Step Setup

### Step 1: Build the Anchor Program (On-Chain)

The `anchor build` command can take several minutes. Run it in your terminal:

```bash
cd program
anchor build
```

**Note:** This may take 5-10 minutes on first build as it compiles all Rust dependencies.

**If build hangs or takes too long:**
- Let it run - first builds are slow
- Check your terminal for any error messages
- Make sure you have enough disk space

### Step 2: Build the Client (Off-Chain Bot)

```bash
cd ../client
cargo build --release
```

This builds the arbitrage bot client that will search for opportunities.

### Step 3: Set Up Wallet Configuration

The program needs a wallet keypair. Check the paths in `client/src/main.rs`:

- **For localnet:** `../../mainnet_fork/localnet_owner.key`
- **For mainnet:** `/Users/edgar/.config/solana/uwuU3qc2RwN6CpzfBAhg6wAxiEx138jy5wB3Xvx18Rw.json`

**You'll need to:**
1. Create or update the wallet path for your system
2. Or update the code to point to your wallet

### Step 4: Run the Bot

**For localnet (testing):**
```bash
cd client
cargo run --bin main -- --cluster localnet
```

**For mainnet (production):**
```bash
cd client
cargo run --bin main -- --cluster mainnet
```

**Or use the provided script:**
```bash
cd client
./arbitrage.sh
```

This script runs the bot continuously and logs to `log.txt`.

## Troubleshooting

### Build Issues

If `anchor build` fails:
1. Check that Solana CLI v1.9.13 is active: `solana --version`
2. Verify `cargo build-bpf` works: `cargo build-bpf --version`
3. Make sure you're in the `program/` directory

### Wallet Issues

If you get wallet/keypair errors:
1. Create the wallet file at the expected path
2. Or update `client/src/main.rs` line 63-66 to point to your wallet
3. Make sure the wallet has SOL for transactions

### Runtime Issues

If the bot doesn't find opportunities:
- Check RPC connection (mainnet RPC might be rate-limited)
- Verify pool JSON files exist in `pools/` directory
- Check logs for errors

## Quick Start (All Commands)

```bash
# 1. Build Anchor program (takes 5-10 minutes)
cd program
anchor build

# 2. Build client
cd ../client
cargo build --release

# 3. Run on localnet
cargo run --bin main -- --cluster localnet

# OR run on mainnet (make sure wallet is configured)
cargo run --bin main -- --cluster mainnet
```

## What Each Component Does

- **`program/`**: On-chain Solana program that executes swaps atomically
- **`client/`**: Off-chain bot that searches for arbitrage opportunities and calls the program
- **`pools/`**: DEX pool metadata (Orca, Serum, Saber, Mercurial, Aldrin)
- **`mainnet-fork/`**: Tools for testing with mainnet fork

## Notes

- The bot searches for arbitrage opportunities across multiple DEXes
- It uses a brute-force approach to find all opportunities
- The on-chain program ensures atomic execution of multi-hop swaps
- Make sure you have sufficient SOL in your wallet for transaction fees

