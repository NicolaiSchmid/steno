#!/usr/bin/env bash
# The lost display on Linux: runs the built shell on an Xvfb server of its
# own with a fresh HOME, starts a recording from the main window
# (Ctrl+Shift+R, the record shortcut), lets it run, then ends the Xvfb
# server by the PID this script recorded. GDK ends the process when its X
# server goes; the shell's log writer (`display_lost.rs`) must save the
# recording first. Exits 0 when the app logged its save ("the display
# closed; saving") and the store holds the meeting `queued` with a duration above
# zero, 1 otherwise, 2 on a usage error or a missing tool.
#
#   scripts/pipewire-headless.sh \
#     apps/desktop/scripts/lost-display-linux.sh [path/to/steno-desktop] [seconds]
#
# `seconds` (default 4) is how long the recording runs before the server
# ends. Run it inside scripts/pipewire-headless.sh, which gives the
# recorder a microphone and an output to capture. Needs Xvfb, xdotool and
# python3 (to read the store), and the web dist embedded as for the smoke
# (smoke-linux.sh). The app's log stays in the work directory it prints.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
binary="${1:-$root/target/debug/steno-desktop}"
seconds="${2:-4}"

[[ -x "$binary" ]] || { echo "lost-display: $binary is not an executable" >&2; exit 2; }
[[ "$seconds" =~ ^[1-9][0-9]*$ ]] \
  || { echo "lost-display: seconds must be a positive number, got \"$seconds\"" >&2; exit 2; }
for tool in Xvfb xdotool python3; do
  command -v "$tool" >/dev/null || { echo "lost-display: $tool is not on PATH" >&2; exit 2; }
done

work="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/steno-lost-display.XXXXXX")"
echo "lost-display: work directory $work"
server=""
app=""
# Ends only the processes this script started.
cleanup() {
  for pid in "$app" "$server"; do
    [[ -z "$pid" ]] || kill -KILL "$pid" 2>/dev/null || true
  done
}
trap cleanup EXIT

fail() {
  echo "lost-display: FAILED, $*" >&2
  echo "lost-display: the app's log:" >&2
  sed 's/^/  /' "$work/app.log" >&2 || true
  exit 1
}

# The meeting rows the store holds, one "state duration" line each.
meetings() {
  python3 - "$1" <<'EOF'
import sqlite3, sys
try:
    store = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True, timeout=5)
    for state, duration in store.execute("select state, duration from meeting"):
        print(state, duration)
except sqlite3.Error:
    pass
EOF
}

# Xvfb writes the display it took to fd 3 once it is ready.
Xvfb -displayfd 3 -screen 0 2200x1500x24 -nolisten tcp \
  3>"$work/display" >"$work/server.log" 2>&1 &
server=$!
for _ in $(seq 100); do
  [[ -s "$work/display" ]] && break
  sleep 0.1
done
[[ -s "$work/display" ]] || fail "Xvfb did not start"

mkdir -p "$work/home"
export HOME="$work/home"
export XDG_DATA_HOME="$HOME/.local/share" XDG_CONFIG_HOME="$HOME/.config" XDG_CACHE_HOME="$HOME/.cache"
DISPLAY=":$(cat "$work/display")"
export DISPLAY GDK_BACKEND=x11
unset WAYLAND_DISPLAY XDG_SESSION_TYPE
# Software rendering, as in smoke-linux.sh.
export WEBKIT_DISABLE_DMABUF_RENDERER=1 WEBKIT_DISABLE_COMPOSITING_MODE=1 LIBGL_ALWAYS_SOFTWARE=1
export RUST_LOG=info
store="$XDG_DATA_HOME/Steno/steno.sqlite"

"$binary" >"$work/app.log" 2>&1 &
app=$!

main=""
for _ in $(seq 120); do
  main="$(xdotool search --name '^Steno$' 2>/dev/null | head -n 1 || true)"
  [[ -n "$main" ]] && break
  kill -0 "$app" 2>/dev/null || fail "the app ended before its main window showed"
  sleep 0.5
done
[[ -n "$main" ]] || fail "no main window within 60 s"

# The page may still be loading, so the shortcut is sent again while the
# store holds no meeting at all: a press after a recording started would
# stop it.
recording=""
for _ in $(seq 6); do
  if [[ -z "$(meetings "$store")" ]]; then
    xdotool windowfocus --sync "$main" 2>/dev/null || true
    xdotool key --clearmodifiers ctrl+shift+r
  fi
  for _ in $(seq 20); do
    if grep -q '^recording ' <<<"$(meetings "$store")"; then
      recording=yes
      break 2
    fi
    sleep 0.5
  done
done
[[ -n "$recording" ]] || fail "no recording started"
echo "lost-display: recording; ending the display server in $seconds s"
sleep "$seconds"

kill -TERM "$server"
for _ in $(seq 300); do
  kill -0 "$app" 2>/dev/null || break
  sleep 0.1
done
kill -0 "$app" 2>/dev/null && fail "the app was still running 30 s after the display server ended"
code=0
wait "$app" || code=$?
app=""
echo "lost-display: the app exited with $code"

grep -qF "the display closed; saving" "$work/app.log" \
  || fail "the app did not save for the lost display"
rows="$(meetings "$store")"
echo "lost-display: the store holds: ${rows:-nothing}"
[[ -n "$rows" && "$(wc -l <<<"$rows")" == 1 ]] || fail "expected one meeting"
read -r state duration <<<"$rows"
[[ "$state" == queued ]] || fail "the meeting is $state, not queued"
python3 -c 'import sys; sys.exit(0 if float(sys.argv[1]) > 0 else 1)' "$duration" \
  || fail "the meeting has no duration ($duration)"
echo "lost-display: ok, the recording was saved before the app ended"
