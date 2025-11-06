# Alternative Solana CLI Installation Methods

## Problem: SSL Connection Error

If you encounter:
```
curl: (35) LibreSSL SSL_connect: SSL_ERROR_SYSCALL in connection to release.solana.com:443
```

This can be due to:
- Network/firewall issues
- SSL certificate problems
- Outdated curl/LibreSSL
- Proxy settings

## Solution 1: Retry with Different Options

```bash
# Try with insecure flag (not recommended for production)
curl -k -sSfL https://release.solana.com/v1.9.13/install | sh

# Or try with verbose output to see what's failing
curl -v https://release.solana.com/v1.9.13/install
```

## Solution 2: Manual Installation

### Step 1: Download the installer script manually

```bash
# Download the installer
curl -L https://release.solana.com/v1.9.13/install -o /tmp/solana-install.sh

# Make it executable
chmod +x /tmp/solana-install.sh

# Run it
/tmp/solana-install.sh
```

### Step 2: Or download binaries directly

```bash
# Create Solana directory
mkdir -p ~/.local/share/solana/install/releases/1.9.13

# Download binaries (you'll need to find the correct URL for your platform)
# For macOS ARM64:
curl -L https://github.com/solana-labs/solana/releases/download/v1.9.13/solana-release-aarch64-apple-darwin.tar.bz2 -o /tmp/solana.tar.bz2

# Extract
cd ~/.local/share/solana/install/releases/1.9.13
tar -xjf /tmp/solana.tar.bz2

# Create symlink
ln -s ~/.local/share/solana/install/releases/1.9.13 ~/.local/share/solana/install/active_release

# Add to PATH
export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"
echo 'export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"' >> ~/.zshrc
```

## Solution 3: Use Homebrew (if available)

```bash
# Check if you have Homebrew
which brew

# If you have Homebrew, you might be able to install via:
# Note: Homebrew may not have v1.9.13, but you can try
brew install solana
```

## Solution 4: Install via Cargo (if Solana is available as a crate)

```bash
# Check if cargo-install-solana exists
cargo search solana-install

# Or try installing build-bpf directly
cargo install cargo-build-bpf --version 1.9.13
```

## Solution 5: Check Network/Proxy Settings

```bash
# Check if you're behind a proxy
echo $http_proxy
echo $https_proxy
echo $HTTP_PROXY
echo $HTTPS_PROXY

# If you have proxy settings, you may need to configure curl
# Or temporarily unset them:
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY
```

## Solution 6: Update curl/LibreSSL

```bash
# Check curl version
curl --version

# If outdated, update via Homebrew:
brew install curl

# Or update LibreSSL
brew install openssl
```

## Solution 7: Use wget instead of curl

```bash
# If you have wget installed
wget https://release.solana.com/v1.9.13/install -O /tmp/solana-install.sh
chmod +x /tmp/solana-install.sh
/tmp/solana-install.sh
```

## Solution 8: Install from GitHub Releases

```bash
# Download directly from GitHub releases
# For macOS ARM64:
curl -L https://github.com/solana-labs/solana/releases/download/v1.9.13/solana-release-aarch64-apple-darwin.tar.bz2 -o /tmp/solana-1.9.13.tar.bz2

# Extract
mkdir -p ~/.local/share/solana/install/releases/1.9.13
cd ~/.local/share/solana/install/releases/1.9.13
tar -xjf /tmp/solana-1.9.13.tar.bz2

# Create symlink
ln -s ~/.local/share/solana/install/releases/1.9.13 ~/.local/share/solana/install/active_release

# Add to PATH
export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"
echo 'export PATH="$HOME/.local/share/solana/install/active_release/bin:$PATH"' >> ~/.zshrc

# Reload shell
source ~/.zshrc
```

## Verification

After installation, verify:

```bash
solana --version
# Should show: solana-cli 1.9.13

cargo build-bpf --version
# Should show the build-bpf tool

which solana
# Should show the path to solana binary
```

## Troubleshooting

If installation succeeds but `build-bpf` is still not found:

```bash
# Check if cargo-build-bpf is installed
cargo install --list | grep build-bpf

# If not, install it manually
cargo install cargo-build-bpf --version 1.9.13

# Or install from source
git clone --depth 1 --branch v1.9.13 https://github.com/solana-labs/solana.git /tmp/solana-1.9.13
cd /tmp/solana-1.9.13/cargo-build-bpf
cargo install --path .
```

