#!/usr/bin/env bash
# Run all probes in order. Each probe is allowed to fail; we want the
# raw output captured either way.
set -uo pipefail
SPIKE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

for p in "$SPIKE_DIR"/probes/[0-9]*.sh; do
    bash "$p" || echo "(probe $p exited non-zero — see results/)"
done

echo
echo "All probes done. Inspect $SPIKE_DIR/results/ and write FINDINGS.md."
