#!/bin/sh
# cargo build --release # re-compile 

# Configuration - UPDATE THESE PATHS
WALLET_PATH="../mainnet-wallet.json"
RPC_URL="https://api.mainnet-beta.solana.com"

# Dry run mode: Set to true to only log opportunities, don't execute transactions
# Set to false to execute real transactions
DRY_RUN=true

# WebSocket mode: Set to true to enable websocket and log to log.txt
# Set to false to disable websocket and log to console only
ENABLE_WEBSOCKET=true

# Create log file if it doesn't exist, or clear it if it does
if [ -f log.txt ]; then
    rm log.txt
fi
touch log.txt

echo "Starting arbitrage bot..."
if [ "$ENABLE_WEBSOCKET" = "true" ]; then
    echo "Log file: $(pwd)/log.txt (WebSocket mode - logging enabled)"
else
    echo "Logging: Console only (WebSocket disabled)"
fi
echo "Wallet: $WALLET_PATH"
echo "RPC: $RPC_URL"
echo "Dry Run Mode: $DRY_RUN (set DRY_RUN=false in script to execute real transactions)"
echo "WebSocket Mode: $ENABLE_WEBSOCKET"
echo ""

# Run bot continuously (bot has its own infinite loop with websocket support)
# Bot will run indefinitely with websocket real-time updates
# Build command with dry-run flag if enabled
DRY_RUN_FLAG=""
if [ "$DRY_RUN" = "true" ]; then
    DRY_RUN_FLAG="--dry-run"
fi

# WebSocket flag
WEBSOCKET_FLAG=""
if [ "$ENABLE_WEBSOCKET" = "true" ]; then
    WEBSOCKET_FLAG="--websocket"
fi

# Pool whitelist flag (optional - set path to whitelist file or leave empty to use all pools)
# Example: POOL_WHITELIST="../pool_whitelist.json"
POOL_WHITELIST="../pool_whitelist_test.json"
POOL_WHITELIST_FLAG=""
if [ -n "$POOL_WHITELIST" ] && [ -f "$POOL_WHITELIST" ]; then
    POOL_WHITELIST_FLAG="--pool-whitelist $POOL_WHITELIST"
    echo "Using pool whitelist: $POOL_WHITELIST"
fi

# Run bot once - it has its own infinite loop for websocket updates
# Only log to log.txt when websocket mode is enabled and connected
if [ "$ENABLE_WEBSOCKET" = "true" ]; then
    # Use awk to buffer output and only start logging to file after websocket connection is confirmed
    # This ensures log.txt only contains data when websocket mode is active
    RUST_LOG=info ./target/release/main --cluster mainnet --wallet "$WALLET_PATH" --rpc-url "$RPC_URL" $WEBSOCKET_FLAG $DRY_RUN_FLAG $POOL_WHITELIST_FLAG 2>&1 | awk '
    BEGIN { websocket_active = 0; }
    /SWITCHING TO WEBSOCKET MODE|WEBSOCKET CONNECTED|ALL.*SUBSCRIPTIONS CONFIRMED|WEBSOCKET MODE ACTIVE/ { 
        websocket_active = 1; 
        print > "log.txt";
        next;
    }
    websocket_active == 1 && /found arbitrage|arb already sent|Search cycle completed|SWITCHING|WEBSOCKET|WebSocket|websocket|Pool registry|Initializing|WebSocket manager|WebSocket connected|subscription|WebSocket update|subscription confirmed|CONNECTED|Connection established|Subscribing|subscriptions confirmed|ALL.*SUBSCRIPTIONS CONFIRMED|WEBSOCKET MODE ACTIVE|ERROR|error|WARN|warn/ {
        print >> "log.txt";
    }
    websocket_active == 0 {
        # Before websocket is active, print to console only
        print;
    }
    '
else
    # Log to console when websocket is disabled
    RUST_LOG=info ./target/release/main --cluster mainnet --wallet "$WALLET_PATH" --rpc-url "$RPC_URL" $WEBSOCKET_FLAG $DRY_RUN_FLAG $POOL_WHITELIST_FLAG
fi