#!/usr/bin/env bash
# Probe 5: per-character isolation.
#
# We try two things:
#  A. HOME=<tempdir>: does claude still authenticate? If not, OAuth
#     creds live in HOME, so per-character HOME means re-auth per
#     character — unworkable.
#  B. --setting-sources "" + a fresh CWD: does that sufficiently
#     keep user/project/local settings out without breaking auth?
#
# Findings inform whether shore can run multiple characters in
# parallel without their state bleeding.
set -euo pipefail
source "$(dirname "$0")/_common.sh"

OUT_A="$RESULTS_DIR/05a-home-tempdir.txt"
OUT_B="$RESULTS_DIR/05b-setting-sources.txt"

banner "Probe 05a: HOME=<tempdir> — does auth survive?"
TMP_HOME="$(mktemp -d)"
echo "Using HOME=$TMP_HOME"
HOME="$TMP_HOME" claude auth status 2>&1 > "$OUT_A" || true
cat "$OUT_A"

banner "Probe 05b: --setting-sources \"\" with normal HOME"
claude --print \
    --output-format json \
    --no-session-persistence \
    --setting-sources "" \
    --model "$PROBE_MODEL" \
    --tools "" \
    --system-prompt "You are a test fixture. Reply with exactly the word OK and nothing else." \
    "ack" > "$OUT_B" 2>&1 || true
cat "$OUT_B"
