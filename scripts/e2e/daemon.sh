#!/usr/bin/env bash
#
# scripts/e2e/daemon.sh — a runnable, fully isolated shore-daemon for e2e checks.
#
# Spins up a real shore-daemon (current local build) in a throwaway profile so you
# can exercise a feature end-to-end — arbitrary config including [mcp.*] servers and
# sub-agents, real models — WITHOUT touching your live daemon, its conversation,
# memory, port, or LLM-sidecar socket.
#
# Isolation (see ARCHITECTURE.md / scripts/e2e/README.md):
#   - Own SHORE_CONFIG_DIR / SHORE_DATA_DIR / SHORE_RUNTIME_DIR under a temp dir, so
#     its instances.json AND its llm.sock (runtime.join("llm.sock")) are isolated.
#   - Binds 127.0.0.1:0 (kernel-assigned port); the CLI always talks to it via an
#     explicit --addr, never instance discovery, so it can't reach the live daemon.
#   - Autonomy off and no [connections.matrix.*] by default — no heartbeats, no
#     port-6167 collision.
#   - Provider keys come from a copy of your real ~/.config/shore/.env + conf.d/.
#
# Usage:
#   daemon.sh up   [--name N] [--config FILE] [--character FILE] [--model REF] [--release]
#   daemon.sh send [--name N] "message"
#   daemon.sh exec [--name N] -- <shore args...>     # any shore subcommand vs the instance
#   daemon.sh logs [--name N]                        # tail the daemon log
#   daemon.sh list                                   # running e2e instances
#   daemon.sh down [--name N]                         # stop + delete the instance
#
# Example (verify ask_music end-to-end):
#   daemon.sh up --name music \
#       --config    scripts/e2e/examples/music.toml \
#       --character scripts/e2e/examples/music-soul.md
#   daemon.sh send --name music "What have I played most this month, and what do critics say about it?"
#   daemon.sh exec --name music -- log            # inspect the turn + tool calls
#   daemon.sh down --name music
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
REAL_CONFIG_DIR="${SHORE_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/shore}"
STATE_ROOT="${XDG_STATE_HOME:-$HOME/.local/state}/shore-e2e"

DEFAULT_NAME="default"
DEFAULT_MODEL="opencode-go:glm-5.2"   # subscription/$0; override with --model
CHAR_NAME="e2e"

# ── Colors ──────────────────────────────────────────────────────────
if [[ -t 1 ]]; then
    RED=$'\033[0;31m'; GRN=$'\033[0;32m'; YLW=$'\033[0;33m'; CYN=$'\033[0;36m'; DIM=$'\033[0;90m'; NC=$'\033[0m'
else
    RED=""; GRN=""; YLW=""; CYN=""; DIM=""; NC=""
fi
say()  { echo -e "${CYN}[e2e]${NC} $*"; }
warn() { echo -e "${YLW}[e2e] $*${NC}" >&2; }
die()  { echo -e "${RED}[e2e] $*${NC}" >&2; exit 1; }

# Reject instance names that could escape STATE_ROOT / the /tmp profile prefix.
validate_name() {
    [[ "$1" =~ ^[A-Za-z0-9_-]+$ ]] || die "invalid --name '$1' (letters, digits, '-' and '_' only)."
}
# Guard option values: a missing value (or the next option mistaken for one)
# dies cleanly instead of tripping set -u or silently swallowing a flag.
need_val() { [[ -n "${2:-}" && "${2:-}" != --* ]] || die "option $1 requires a value."; }

# Confirm a PID is actually our daemon before signaling it — guards against the
# OS having recycled a stale PID to an unrelated process.
pid_matches_instance() {
    local cmd; cmd="$(ps -p "$1" -o command= 2>/dev/null || true)"
    [[ "$cmd" == *shore-daemon* && "$cmd" == *"--instance-id $2"* ]]
}

state_file() { echo "$STATE_ROOT/$1.json"; }

# Read one field from a state file via python3 (no jq dependency).
state_get() { python3 -c "import json,sys; print(json.load(open(sys.argv[1]))[sys.argv[2]])" "$1" "$2"; }

require_instance() {
    local name="$1" f; f="$(state_file "$name")"
    [[ -f "$f" ]] || die "no e2e instance named '$name' — run 'daemon.sh up' first (see 'daemon.sh list')."
    E_TMP="$(state_get "$f" tmpdir)"
    E_ADDR="$(state_get "$f" addr)"
    E_PID="$(state_get "$f" pid)"
    E_CHAR="$(state_get "$f" character)"
    E_INSTANCE="$(state_get "$f" instance)"
}

# ── up ──────────────────────────────────────────────────────────────
cmd_up() {
    local name="$DEFAULT_NAME" config="" character="" model="$DEFAULT_MODEL" profile="debug"
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --name)      need_val "$1" "${2:-}"; name="$2"; shift 2 ;;
            --config)    need_val "$1" "${2:-}"; config="$2"; shift 2 ;;
            --character) need_val "$1" "${2:-}"; character="$2"; shift 2 ;;
            --model)     need_val "$1" "${2:-}"; model="$2"; shift 2 ;;
            --release)   profile="release"; shift ;;
            *) die "up: unknown arg '$1'" ;;
        esac
    done
    validate_name "$name"

    local f; f="$(state_file "$name")"
    if [[ -f "$f" ]]; then
        local existing_pid; existing_pid="$(state_get "$f" pid)"
        if kill -0 "$existing_pid" 2>/dev/null && pid_matches_instance "$existing_pid" "e2e-$name"; then
            die "e2e instance '$name' is already up (pid $existing_pid). 'daemon.sh down --name $name' first."
        fi
        # Stale state (process gone, or PID recycled to something else) — overwrite.
    fi
    [[ -z "$config"    || -f "$config"    ]] || die "--config file not found: $config"
    [[ -z "$character" || -f "$character" ]] || die "--character file not found: $character"

    say "building shore-daemon + shore ($profile)..."
    local build_flags=(-p shore-daemon -p shore-cli)
    [[ "$profile" == "release" ]] && build_flags+=(--release)
    cargo build --manifest-path "$REPO_ROOT/Cargo.toml" "${build_flags[@]}" 2>&1 | tail -3
    local daemon_bin="$REPO_ROOT/target/$profile/shore-daemon"
    local shore_bin="$REPO_ROOT/target/$profile/shore"
    [[ -x "$daemon_bin" ]] || die "missing $daemon_bin"
    [[ -x "$shore_bin"  ]] || die "missing $shore_bin"

    local tmp; tmp="$(mktemp -d "/tmp/shore-e2e-${name}-XXXXXX")"
    local cfg_dir="$tmp/config" data_dir="$tmp/data" rt_dir="$tmp/runtime" cache_dir="$tmp/cache"
    local char_dir="$cfg_dir/characters/$CHAR_NAME/workspace"
    mkdir -p "$cfg_dir/conf.d" "$data_dir" "$rt_dir" "$cache_dir" "$char_dir/files" "$char_dir/memory"

    # Provider keys + provider/model definitions, copied from the real profile.
    [[ -f "$REAL_CONFIG_DIR/.env" ]] && cp "$REAL_CONFIG_DIR/.env" "$cfg_dir/.env"
    if compgen -G "$REAL_CONFIG_DIR/conf.d/*.toml" >/dev/null; then
        cp "$REAL_CONFIG_DIR"/conf.d/*.toml "$cfg_dir/conf.d/"
    fi

    # Main config: caller-supplied (may carry [mcp.*]/[subagents.*]/[tools]) or minimal.
    if [[ -n "$config" ]]; then
        cp "$config" "$cfg_dir/config.toml"
    else
        cat > "$cfg_dir/config.toml" <<'TOML'
[defaults]
display_name = "e2e"
TOML
    fi

    # Isolation overrides — conf.d deep-merges OVER config.toml (core/config/src/lib.rs:415),
    # so these win regardless of what the caller config set.
    cat > "$cfg_dir/conf.d/_e2e_override.toml" <<TOML
[defaults]
model = "$model"

[behavior.autonomy]
enabled = false

[daemon]
addr = "127.0.0.1:0"

# Full request/response payloads (incl. tool_use blocks) to the data dir, so e2e
# checks can trace exactly which tools fired.
[advanced]
api_payload_logging = true
TOML

    # Character persona.
    if [[ -n "$character" ]]; then
        cp "$character" "$char_dir/SOUL.md"
    else
        cat > "$char_dir/SOUL.md" <<'MD'
# e2e

A throwaway end-to-end test character. Be terse. When asked to use a tool or
sub-agent, do so directly and report what happened.
MD
    fi

    local instance="e2e-$name"
    say "starting daemon (instance=$instance, profile=$profile, model=$model)..."
    SHORE_CONFIG_DIR="$cfg_dir" SHORE_DATA_DIR="$data_dir" SHORE_RUNTIME_DIR="$rt_dir" \
    SHORE_CACHE_DIR="$cache_dir" \
        "$daemon_bin" --instance-id "$instance" --addr "127.0.0.1:0" \
        > "$tmp/daemon.log" 2>&1 &
    local pid=$!

    # Poll the isolated registry for the kernel-assigned addr (see spawn_bind_zero.rs).
    local addr="" deadline=$((SECONDS + 30))
    while [[ $SECONDS -lt $deadline ]]; do
        if ! kill -0 "$pid" 2>/dev/null; then
            warn "daemon exited during startup; log tail:"; tail -20 "$tmp/daemon.log" >&2
            rm -rf "$tmp"; die "startup failed."
        fi
        if [[ -f "$rt_dir/instances.json" ]]; then
            addr="$(python3 - "$rt_dir/instances.json" "$instance" <<'PY' || true
import json,sys
try:
    for e in json.load(open(sys.argv[1])):
        if e.get("id")==sys.argv[2] and e.get("addr") and not e["addr"].endswith(":0"):
            print(e["addr"]); break
except Exception:
    pass
PY
)"
            [[ -n "$addr" ]] && break
        fi
        sleep 0.3
    done
    [[ -n "$addr" ]] || { tail -20 "$tmp/daemon.log" >&2; kill "$pid" 2>/dev/null || true; rm -rf "$tmp"; die "daemon did not register an address within 30s."; }

    mkdir -p "$STATE_ROOT"
    python3 - "$f" "$tmp" "$addr" "$pid" "$instance" "$CHAR_NAME" "$shore_bin" <<'PY'
import json,sys
keys=["_","f","tmpdir","addr","pid","instance","character","shore_bin"]
_,f,tmpdir,addr,pid,instance,character,shore_bin=sys.argv
json.dump({"tmpdir":tmpdir,"addr":addr,"pid":int(pid),"instance":instance,
          "character":character,"shore_bin":shore_bin}, open(f,"w"), indent=2)
PY

    say "${GRN}up${NC}  name=$name  addr=$addr  pid=$pid"
    echo -e "    ${DIM}profile=$tmp${NC}"
    echo -e "    ${DIM}drive:  $0 send --name $name \"hello\"${NC}"
    echo -e "    ${DIM}stop:   $0 down --name $name${NC}"
}

# ── send / exec / logs / down / list ────────────────────────────────
cmd_send() {
    local name="$DEFAULT_NAME"
    [[ "${1:-}" == "--name" ]] && { need_val --name "${2:-}"; name="$2"; shift 2; }
    validate_name "$name"
    [[ $# -ge 1 ]] || die "send: need a message"
    require_instance "$name"
    "$(state_get "$(state_file "$name")" shore_bin)" --addr "$E_ADDR" -c "$E_CHAR" send "$*"
}

cmd_exec() {
    local name="$DEFAULT_NAME"
    [[ "${1:-}" == "--name" ]] && { need_val --name "${2:-}"; name="$2"; shift 2; }
    validate_name "$name"
    [[ "${1:-}" == "--" ]] && shift
    [[ $# -ge 1 ]] || die "exec: need a shore subcommand, e.g. exec -- status"
    require_instance "$name"
    "$(state_get "$(state_file "$name")" shore_bin)" --addr "$E_ADDR" -c "$E_CHAR" "$@"
}

cmd_logs() {
    local name="$DEFAULT_NAME"
    [[ "${1:-}" == "--name" ]] && { need_val --name "${2:-}"; name="$2"; shift 2; }
    validate_name "$name"
    require_instance "$name"
    tail -n 80 -f "$E_TMP/daemon.log"
}

cmd_down() {
    local name="$DEFAULT_NAME"
    [[ "${1:-}" == "--name" ]] && { need_val --name "${2:-}"; name="$2"; shift 2; }
    validate_name "$name"
    local f; f="$(state_file "$name")"
    [[ -f "$f" ]] || { warn "no e2e instance named '$name'."; return 0; }
    require_instance "$name"
    if kill -0 "$E_PID" 2>/dev/null && pid_matches_instance "$E_PID" "$E_INSTANCE"; then
        # Wait for the daemon to actually exit before removing its profile —
        # otherwise its shutdown can recreate the runtime dir after the rm, and
        # tearing down a still-running daemon would orphan it.
        kill "$E_PID" 2>/dev/null || true
        for _ in $(seq 1 50); do kill -0 "$E_PID" 2>/dev/null || break; sleep 0.1; done
        if kill -0 "$E_PID" 2>/dev/null; then
            warn "pid $E_PID ignored SIGTERM; escalating to SIGKILL."
            kill -9 "$E_PID" 2>/dev/null || true
            for _ in $(seq 1 20); do kill -0 "$E_PID" 2>/dev/null || break; sleep 0.1; done
        fi
        kill -0 "$E_PID" 2>/dev/null && \
            die "pid $E_PID is still running — leaving its profile and state intact; kill it manually and retry 'down'."
        say "stopped pid $E_PID"
    elif kill -0 "$E_PID" 2>/dev/null; then
        # PID is alive but not our daemon (recycled) — never signal it; just
        # clean up our own stale profile/state.
        warn "pid $E_PID is not the '$E_INSTANCE' daemon (PID reused?); not signaling it, cleaning up stale state only."
    fi
    [[ -n "$E_TMP" && "$E_TMP" == /tmp/shore-e2e-* ]] && rm -rf "$E_TMP"
    rm -f "$f"
    say "${GRN}down${NC} name=$name (profile cleaned up)"
}

cmd_list() {
    [[ -d "$STATE_ROOT" ]] || { say "no e2e instances."; return 0; }
    local any=0
    for f in "$STATE_ROOT"/*.json; do
        [[ -e "$f" ]] || continue
        any=1
        local n; n="$(basename "$f" .json)"
        local pid; pid="$(state_get "$f" pid)"
        local addr; addr="$(state_get "$f" addr)"
        if kill -0 "$pid" 2>/dev/null; then
            echo -e "  ${GRN}●${NC} $n  addr=$addr  pid=$pid"
        else
            echo -e "  ${RED}○${NC} $n  (dead pid=$pid — 'down --name $n' to clean up)"
        fi
    done
    [[ $any -eq 1 ]] || say "no e2e instances."
}

case "${1:-}" in
    up)   shift; cmd_up "$@" ;;
    send) shift; cmd_send "$@" ;;
    exec) shift; cmd_exec "$@" ;;
    logs) shift; cmd_logs "$@" ;;
    down) shift; cmd_down "$@" ;;
    list) shift; cmd_list "$@" ;;
    -h|--help|help|"") sed -n '2,40p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' ;;
    *) die "unknown command '$1' (try: up|send|exec|logs|list|down)" ;;
esac
