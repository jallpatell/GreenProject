#!/bin/sh
# cargo build --release # re-compile 

# Configuration - UPDATE THESE PATHS
WALLET_PATH="/Users/jal/.config/solana/testnet-keypair.json"
RPC_URL="https://api.mainnet-beta.solana.com"

# Create log file if it doesn't exist, or clear it if it does
if [ -f log.txt ]; then
    rm log.txt
fi
touch log.txt

echo "Starting arbitrage bot..."
echo "Log file: $(pwd)/log.txt"
echo "Wallet: $WALLET_PATH"
echo "RPC: $RPC_URL"
echo ""

# continuously search for arbitrages
# Only log arbitrage opportunities - no timestamps, no debug messages
while true
do
    RUST_LOG=info ./target/release/main --cluster mainnet --wallet "$WALLET_PATH" --rpc-url "$RPC_URL" 2>&1 | grep -E "(found arbitrage|arb already sent|Search cycle completed|Fetching pool amounts|Pool data fetched|Added starting token|Will search from|Searching from|ERROR|error)" >> log.txt
    sleep 1  # Small delay between runs
done