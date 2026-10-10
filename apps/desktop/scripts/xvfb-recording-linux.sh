# shellcheck shell=bash disable=SC2154 # `name` is the sourcing script's
# What lost-display-linux.sh and close-without-tray-linux.sh share; each
# sets `name`, its log prefix, and sources this file with its arguments
# ([path/to/steno-desktop] [seconds]). Sourcing it checks them and the
# tools (Xvfb, xdotool, python3; exit 2 on a usage error or a missing
# tool), makes the work directory and ends the processes started from it
# on exit. `start_recording` runs the built shell on an Xvfb server of
# its own with a fresh HOME and starts a recording from the main window
# (Ctrl+Shift+R, the record shortcut); `await_exit` waits for the app to
# end; `require_saved` checks the store holds the recording, saved.

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
binary="${1:-$root/target/debug/steno-desktop}"
seconds="${2:-4}"

[[ -x "$binary" ]] || { echo "$name: $binary is not an executable" >&2; exit 2; }
[[ "$seconds" =~ ^[1-9][0-9]*$ ]] \
  || { echo "$name: seconds must be a positive number, got \"$seconds\"" >&2; exit 2; }
for tool in Xvfb xdotool python3; do
  command -v "$tool" >/dev/null || { echo "$name: $tool is not on PATH" >&2; exit 2; }
done

work="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/steno-$name.XXXXXX")"
echo "$name: work directory $work"
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
  echo "$name: FAILED, $*" >&2
  echo "$name: the app's log:" >&2
  sed 's/^/  /' "$work/app.log" >&2 || true
  exit 1
}

# The meeting rows the store holds, one "state duration endReason" line
# each; the store keeps the end reason as JSON (`"quit"`).
meetings() {
  python3 - "$1" <<'EOF'
import json, sqlite3, sys
try:
    store = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True, timeout=5)
    for state, duration, reason in store.execute(
        "select state, duration, endReason from meeting"
    ):
        try:
            reason = json.loads(reason) if reason else reason
        except ValueError:
            pass
        print(state, duration, reason)
except sqlite3.Error:
    pass
EOF
}

# Starts Xvfb and the app on it (`server`, `app`, its log in
# `$work/app.log`), finds its main window (`main`) and starts a recording
# there; the store is `store`.
start_recording() {
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
    main="$(xdotool search --onlyvisible --name '^Steno$' 2>/dev/null | head -n 1 || true)"
    [[ -n "$main" ]] && break
    kill -0 "$app" 2>/dev/null || fail "the app ended before its main window showed"
    sleep 0.5
  done
  [[ -n "$main" ]] || fail "no main window within 60 s"

  # The page may still be loading, so the shortcut is sent again while the
  # store holds no meeting at all: a press after a recording started would
  # stop it.
  local recording=""
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
}

# Waits 30 s at most for the app to end after $1, and keeps its exit
# status in `code`.
await_exit() {
  for _ in $(seq 300); do
    kill -0 "$app" 2>/dev/null || break
    sleep 0.1
  done
  kill -0 "$app" 2>/dev/null && fail "the app was still running 30 s after $1"
  code=0
  wait "$app" || code=$?
  app=""
  echo "$name: the app exited with $code"
}

# The store must hold one meeting, `queued` with a duration above zero;
# its end reason is kept in `reason`.
require_saved() {
  local rows state duration
  rows="$(meetings "$store")"
  echo "$name: the store holds: ${rows:-nothing}"
  [[ -n "$rows" && "$(wc -l <<<"$rows")" == 1 ]] || fail "expected one meeting"
  # shellcheck disable=SC2034 # `reason` is for the sourcing script
  read -r state duration reason <<<"$rows"
  [[ "$state" == queued ]] || fail "the meeting is $state, not queued"
  python3 -c 'import sys; sys.exit(0 if float(sys.argv[1]) > 0 else 1)' "$duration" \
    || fail "the meeting has no duration ($duration)"
}
