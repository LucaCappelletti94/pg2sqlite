#!/usr/bin/env bash
# Start every cargo-fuzz target in its own tmux pane.
#
# Targets are discovered dynamically with `cargo +nightly fuzz list`, so
# adding or renaming a target in `fuzz/Cargo.toml` is picked up here on
# the next invocation - no edit to this script required.
#
# Usage:
#   fuzz/run-all.sh                   # start (or attach to) the session
#   fuzz/run-all.sh --kill            # kill the session, drop the artefacts
#   fuzz/run-all.sh -- -max_total_time=600    # extra args appended to defaults
#
# Session is named `pg2sqlite-fuzz`; re-running while it's alive attaches
# instead of restarting fuzzing.

set -euo pipefail

SESSION="pg2sqlite-fuzz"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" &>/dev/null && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

# libFuzzer runtime knobs passed after `--` to every target. Mirrors
# the same defaults the subql fuzzer uses for the same reasons:
#
#   -timeout=15        abort a single input after 15s. Defense in depth
#                      against new exponential-parse pathologies we
#                      haven't yet patched upstream in sqlparser-rs.
#   -max_len=65536     cap generated input size at 64 KiB. Without this,
#                      libFuzzer can produce multi-MB inputs that hit
#                      pathological allocation paths and bloat the
#                      `artefacts/` directory with low-signal cases.
#   -rss_limit_mb=8192 raise libFuzzer's RSS ceiling from the 2 GiB
#                      default. Under ASAN (which cargo-fuzz enables by
#                      default) the allocator fragments and the resident
#                      set drifts past 2 GiB after tens of thousands of
#                      iterations even when no single input is large.
#                      The previous fuzz sessions saved several "OOM"
#                      artefacts that replayed in <100 ms / <50 MB on a
#                      fresh process - all attributable to this drift.
DEFAULT_LIBFUZZER_ARGS=(-timeout=15 -max_len=65536 -rss_limit_mb=8192)

if [[ "${1:-}" == "--kill" ]]; then
    tmux kill-session -t "$SESSION" 2>/dev/null && echo "killed $SESSION" || echo "no $SESSION session"
    exit 0
fi

# Allow extra args to be forwarded to every `cargo fuzz run` invocation
# after a literal `--`. They are appended to DEFAULT_LIBFUZZER_ARGS so
# users can extend (e.g. `-- -max_total_time=600`) or override individual
# flags (libFuzzer applies the last value for repeated keys).
EXTRA_ARGS=()
if [[ "${1:-}" == "--" ]]; then
    shift
    EXTRA_ARGS=("$@")
fi
LIBFUZZER_ARGS=("${DEFAULT_LIBFUZZER_ARGS[@]}" "${EXTRA_ARGS[@]}")

command -v tmux >/dev/null || { echo "tmux not installed" >&2; exit 1; }
command -v cargo-fuzz >/dev/null || { echo "cargo-fuzz not installed (cargo install --locked cargo-fuzz)" >&2; exit 1; }

# Attach if the session already exists.
if tmux has-session -t "$SESSION" 2>/dev/null; then
    echo "session $SESSION already running, attaching"
    exec tmux attach-session -t "$SESSION"
fi

cd "$PROJECT_ROOT"
mapfile -t TARGETS < <(cargo +nightly fuzz list)

if [[ "${#TARGETS[@]}" -eq 0 ]]; then
    echo "no fuzz targets found via 'cargo +nightly fuzz list'" >&2
    exit 1
fi

# Build the libfuzzer arg suffix once.
EXTRA_SUFFIX=" -- ${LIBFUZZER_ARGS[*]}"

# All targets live in a single window as side-by-side panes so a crash
# is visible at a glance without flipping between windows. `tiled`
# layout rebalances after each split so the panes stay equal-sized
# regardless of target count.
WINDOW="fuzz"
first="${TARGETS[0]}"
tmux new-session -d -s "$SESSION" -n "$WINDOW" -c "$PROJECT_ROOT" \
    "cargo +nightly fuzz run $first$EXTRA_SUFFIX; exec bash"

for target in "${TARGETS[@]:1}"; do
    tmux split-window -t "$SESSION:$WINDOW" -c "$PROJECT_ROOT" \
        "cargo +nightly fuzz run $target$EXTRA_SUFFIX; exec bash"
    tmux select-layout -t "$SESSION:$WINDOW" tiled >/dev/null
done

# Show pane borders + target name as the pane title so the operator
# can tell which pane is which when several are running.
tmux set-option -t "$SESSION" -g pane-border-status top >/dev/null
tmux set-option -t "$SESSION" -g pane-border-format ' #{pane_title} ' >/dev/null
i=0
for target in "${TARGETS[@]}"; do
    tmux select-pane -t "$SESSION:$WINDOW.$i" -T "$target"
    i=$((i + 1))
done
tmux select-pane -t "$SESSION:$WINDOW.0"

echo "started session $SESSION with ${#TARGETS[@]} target(s):"
printf '  - %s\n' "${TARGETS[@]}"
echo
echo "attach with: tmux attach -t $SESSION"
echo "kill with:   $0 --kill"

exec tmux attach-session -t "$SESSION"
