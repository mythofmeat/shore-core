#!/usr/bin/env bash
#
# Start/stop shore-daemon for cache testing.
#
# Usage:
#   ./scripts/test-daemon.sh start   # build, start daemon in background
#   ./scripts/test-daemon.sh stop    # kill the backgrounded daemon
#   ./scripts/test-daemon.sh restart # stop + start
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PID_FILE="/tmp/shore-daemon-test.pid"
LOG_FILE="/tmp/shore-test.log"

start_daemon() {
    if [[ -f "$PID_FILE" ]] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
        echo "Daemon already running (PID $(cat "$PID_FILE")). Use 'stop' or 'restart'."
        return 1
    fi

    echo "Building shore-daemon..."
    cargo build --bin shore-daemon --manifest-path "$REPO_ROOT/Cargo.toml" 2>&1 | tail -3

    echo "Starting shore-daemon (log: $LOG_FILE)..."
    RUST_LOG=info,shore_daemon::autonomy=debug,shore_llm_client::providers::anthropic=debug \
        "$REPO_ROOT/target/debug/shore-daemon" \
        > "$LOG_FILE" 2>&1 &

    local pid=$!
    echo "$pid" > "$PID_FILE"
    sleep 0.5

    if kill -0 "$pid" 2>/dev/null; then
        echo "Daemon started (PID $pid)"
    else
        echo "Daemon failed to start. Check $LOG_FILE"
        rm -f "$PID_FILE"
        return 1
    fi
}

stop_daemon() {
    if [[ ! -f "$PID_FILE" ]]; then
        echo "No PID file found. Daemon not running (or started outside this script)."
        return 0
    fi

    local pid
    pid=$(cat "$PID_FILE")
    if kill -0 "$pid" 2>/dev/null; then
        kill "$pid"
        echo "Daemon stopped (PID $pid)"
    else
        echo "Daemon already dead (stale PID $pid)"
    fi
    rm -f "$PID_FILE"
}

case "${1:-}" in
    start)   start_daemon ;;
    stop)    stop_daemon ;;
    restart) stop_daemon; start_daemon ;;
    *)       echo "Usage: $0 {start|stop|restart}"; exit 1 ;;
esac
