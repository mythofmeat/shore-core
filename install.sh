#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")" && pwd)"
BIN_DIR="/usr/local/bin"

echo "Building Rust workspace (release)..."
cargo build --workspace --release --manifest-path "$REPO_DIR/Cargo.toml"

echo "Building shore-llm (TypeScript)..."
cd "$REPO_DIR/shore-llm"
npm install --silent
npm run build

echo "Installing binaries to $BIN_DIR..."
mkdir -p "$BIN_DIR"
for bin in shore-daemon shore shore-tui shore-matrix; do
    cp "$REPO_DIR/target/release/$bin" "$BIN_DIR/$bin"
done

# shore-llm wrapper
cat > "$BIN_DIR/shore-llm" <<'WRAPPER'
#!/bin/sh
exec node /usr/local/lib/shore-llm/dist/index.js "$@"
WRAPPER
chmod 755 "$BIN_DIR/shore-llm"

mkdir -p /usr/local/lib/shore-llm
cp -a "$REPO_DIR/shore-llm/dist" /usr/local/lib/shore-llm/
cp -a "$REPO_DIR/shore-llm/node_modules" /usr/local/lib/shore-llm/
cp "$REPO_DIR/shore-llm/package.json" /usr/local/lib/shore-llm/

echo "Creating config directory..."
mkdir -p "${XDG_CONFIG_HOME:-$HOME/.config}/shore"
mkdir -p "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/shore"

echo ""
echo "Installed:"
for bin in shore-daemon shore shore-tui shore-matrix shore-llm; do
    echo "  $BIN_DIR/$bin"
done
echo ""
echo "Config goes in: ${XDG_CONFIG_HOME:-$HOME/.config}/shore/"
echo "  - config.toml"
echo "  - models.toml (define your model profiles)"
