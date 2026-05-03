#!/usr/bin/env bash
# Shared helpers for spike probes.
set -euo pipefail

SPIKE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RESULTS_DIR="$SPIKE_DIR/results"
mkdir -p "$RESULTS_DIR"

# Render mcp-config.json with the spike dir filled in.
render_mcp_config() {
    local out="$RESULTS_DIR/mcp-config.rendered.json"
    sed "s|__SPIKE_DIR__|$SPIKE_DIR|g" "$SPIKE_DIR/mcp-config.json" > "$out"
    echo "$out"
}

# Probes use a non-thinking-capable default; overridable per probe.
: "${PROBE_MODEL:=claude-sonnet-4-5}"

banner() {
    echo
    echo "===================================================================="
    echo "  $*"
    echo "===================================================================="
}
