#!/usr/bin/env bash
# The tray's Stop while another process holds the store's write lock, on
# Linux: runs the built shell on an Xvfb server of its own with a fresh
# HOME, beside the stand-in tray host (tray-watcher-linux.py), starts a
# recording from the main window (Ctrl+Shift+R, the record shortcut), lets
# it run, then holds the store's write lock for 8 s from Python's sqlite3,
# as the Swift app or any other program with the database open for
# writing can, and clicks Stop recording in the tray's menu.
#
# The menu event runs on the main thread, which every window, the tray
# and the panels draw on, and the stop's save waits for the lock (each try
# waits out the store's 5 s busy timeout). The stop must run off the main
# thread (`actions.rs`), so the tray goes on answering while the save
# waits. Exits 0 when the menu's Record row reads "Stopping…" within 1 s of
# the click, the meeting is `queued` within 2 s of the lock's release, and
# the store then holds that one meeting, saved with a duration above zero.
# There is no log line for a save; the store's row is the save.
#
# Exits 1 when a check fails, 2 on a usage error or a missing tool.
#
#   scripts/pipewire-headless.sh \
#     apps/desktop/scripts/locked-store-linux.sh [path/to/steno-desktop] [seconds]
#
# `seconds` (default 4) is how long the recording runs before the lock is
# taken. Run it inside scripts/pipewire-headless.sh, which gives the
# recorder a microphone and an output to capture, and the app a private
# session bus. Needs Xvfb, xdotool, python3 with its sqlite3 module and
# Python's GObject bindings, and the web dist embedded as for the smoke
# (smoke-linux.sh). The app's log stays in the work directory it prints.
# The steps it shares with close-without-tray-linux.sh are in
# xvfb-recording-linux.sh.
set -euo pipefail

name=locked-store
scripts="$(dirname "${BASH_SOURCE[0]}")"
# shellcheck source=apps/desktop/scripts/xvfb-recording-linux.sh
source "$scripts/xvfb-recording-linux.sh" "$@"
[[ -n "${DBUS_SESSION_BUS_ADDRESS:-}" ]] \
  || { echo "$name: the tray host needs a session bus" >&2; exit 2; }
python3 -c 'import gi, sqlite3' 2>/dev/null \
  || { echo "$name: Python's GObject bindings (python3-gi) or sqlite3 are missing" >&2; exit 2; }

# How long the lock is held, and how soon after the click and after the
# release each check must hold.
hold_seconds=8
stopping_within=1
saved_within=2

watcher=""
holder=""
# Ends the helpers this script started, then what the harness started.
end_all() {
  local pid
  for pid in "$holder" "$watcher"; do
    [[ -z "$pid" ]] || kill -KILL "$pid" 2>/dev/null || true
  done
  cleanup
}
trap end_all EXIT

# Waits 10 s at most for the fixed string $1 in the file $2.
await_line() {
  for _ in $(seq 100); do
    grep -qF "$1" "$2" && return 0
    sleep 0.1
  done
  return 1
}

# The tray's Record row, the menu's first; empty when the menu does not
# answer within the watcher's patience.
record_row() {
  python3 "$scripts/tray-watcher-linux.py" menu "$bus_name" "$item_path" 2>/dev/null \
    | head -n 1 || true
}

now() {
  date +%s.%N
}

# Whether $1 - $2 is at most $3 seconds.
within() {
  python3 -c 'import sys; sys.exit(0 if float(sys.argv[1]) - float(sys.argv[2]) <= float(sys.argv[3]) else 1)' "$@"
}

python3 "$scripts/tray-watcher-linux.py" serve >"$work/watcher.log" 2>&1 &
watcher=$!
await_line "tray-watcher: ready" "$work/watcher.log" \
  || fail "the stand-in watcher did not start: $(cat "$work/watcher.log")"

start_recording
await_line "tray-watcher: item " "$work/watcher.log" \
  || fail "the tray's item did not register with the watcher"
read -r _ _ bus_name item_path < <(grep -F "tray-watcher: item " "$work/watcher.log" | head -n 1)
row=""
for _ in $(seq 50); do
  row="$(record_row)"
  [[ "$row" == "Stop recording" ]] && break
  sleep 0.2
done
[[ "$row" == "Stop recording" ]] || fail "the tray's Record row reads \"$row\", not Stop recording"

echo "$name: recording; holding the store's write lock in $seconds s"
sleep "$seconds"
python3 - "$store" "$hold_seconds" >"$work/lock.log" 2>&1 <<'EOF' &
import sqlite3, sys, time

store = sqlite3.connect(sys.argv[1], timeout=5, isolation_level=None)
store.execute("BEGIN IMMEDIATE")
print("held", flush=True)
time.sleep(float(sys.argv[2]))
store.execute("ROLLBACK")
print(f"released {time.time()}", flush=True)
EOF
holder=$!
await_line "held" "$work/lock.log" || fail "the write lock was not taken: $(cat "$work/lock.log")"

clicked="$(now)"
python3 "$scripts/tray-watcher-linux.py" click "$bus_name" "$item_path" "Stop recording" \
  || fail "Stop recording could not be clicked"
stopping=""
while within "$(now)" "$clicked" "$stopping_within"; do
  row="$(record_row)"
  # Disabled while the recorder is between states (`tray::record_enabled`).
  if [[ "$row" == "Stopping… (disabled)" ]]; then
    stopping="$(now)"
    break
  fi
  sleep 0.05
done
[[ -n "$stopping" ]] \
  || fail "the tray's Record row read \"$row\" $stopping_within s after the click, not Stopping…"
echo "$name: the tray read Stopping… $(python3 -c 'import sys; print(round(float(sys.argv[1]) - float(sys.argv[2]), 2))' "$stopping" "$clicked") s after the click"
grep -q '^recording ' <<<"$(meetings "$store")" \
  || fail "the meeting was saved under the held lock: $(meetings "$store")"

wait "$holder" || fail "the lock's holder failed: $(cat "$work/lock.log")"
holder=""
read -r _ released < <(grep -F "released " "$work/lock.log")
queued=""
while within "$(now)" "$released" "$saved_within"; do
  if grep -q '^queued ' <<<"$(meetings "$store")"; then
    queued="$(now)"
    break
  fi
  sleep 0.1
done
[[ -n "$queued" ]] \
  || fail "the meeting was not queued $saved_within s after the lock's release: $(meetings "$store")"
echo "$name: the meeting was queued $(python3 -c 'import sys; print(round(float(sys.argv[1]) - float(sys.argv[2]), 2))' "$queued" "$released") s after the lock's release"
require_saved
echo "$name: ok, the tray answered while the stop waited for the lock, and the recording was saved"
