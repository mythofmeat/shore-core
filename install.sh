#!/usr/bin/env bash
set -euo pipefail

REPO_DIR="$(cd "$(dirname "$0")" && pwd)"
BIN_DIR="${HOME}/.local/bin"

echo "Building Rust workspace (release)..."
cargo build --workspace --release --manifest-path "$REPO_DIR/Cargo.toml"

echo "Building shore-llm (TypeScript)..."
cd "$REPO_DIR/shore-llm"
npm install --silent
npm run build

echo "Installing binaries to $BIN_DIR..."
mkdir -p "$BIN_DIR"
for bin in shore-daemon shore-cli shore-tui shore-matrix; do
    cp "$REPO_DIR/target/release/$bin" "$BIN_DIR/$bin"
done

echo "Creating config directory..."
mkdir -p "${XDG_CONFIG_HOME:-$HOME/.config}/shore"
mkdir -p "${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/shore"

echo ""
echo "Installed:"
for bin in shore-daemon shore-cli shore-tui shore-matrix; do
    echo "  $BIN_DIR/$bin"
done
echo ""
echo "shore-llm runs from: $REPO_DIR/shore-llm/dist/index.js"
echo ""
echo "Config goes in: ${XDG_CONFIG_HOME:-$HOME/.config}/shore/"
echo "  - config.toml (set [services.llm].command to point to shore-llm)"
echo "  - models.toml (define your model profiles)"
