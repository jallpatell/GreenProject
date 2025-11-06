#!/bin/bash

# Fix AVM symlink issue
# This script creates the symlink from ~/.cargo/bin/anchor to the active AVM version

VERSION=$(cat ~/.avm/.version)
ANCHOR_BINARY="$HOME/.avm/bin/anchor-$VERSION"
SYMLINK_PATH="$HOME/.cargo/bin/anchor"

if [ ! -f "$ANCHOR_BINARY" ]; then
    echo "Error: Anchor binary for version $VERSION not found at $ANCHOR_BINARY"
    echo "Available versions:"
    ls -1 ~/.avm/bin/anchor-* 2>/dev/null | sed 's|.*/anchor-||' || echo "  None found"
    exit 1
fi

# Create ~/.cargo/bin directory if it doesn't exist
mkdir -p "$HOME/.cargo/bin"

# Remove existing symlink or binary if it exists
if [ -L "$SYMLINK_PATH" ] || [ -f "$SYMLINK_PATH" ]; then
    echo "Removing existing anchor at $SYMLINK_PATH"
    rm -f "$SYMLINK_PATH"
fi

# Create symlink
echo "Creating symlink: $SYMLINK_PATH -> $ANCHOR_BINARY"
ln -s "$ANCHOR_BINARY" "$SYMLINK_PATH"

# Verify
if [ -L "$SYMLINK_PATH" ]; then
    echo "✓ Symlink created successfully"
    echo "  Target: $(readlink $SYMLINK_PATH)"
    echo "  Version: $VERSION"
    anchor --version
else
    echo "✗ Failed to create symlink"
    exit 1
fi

