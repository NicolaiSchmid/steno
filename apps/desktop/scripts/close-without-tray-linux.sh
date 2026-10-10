#!/usr/bin/env bash
# Closing the main window with no tray on Linux: runs the built shell on an
# Xvfb server of its own with a fresh HOME and a session bus where no
# status notifier watcher runs (stock GNOME's case), starts a recording
# from the main window (Ctrl+Shift+R, the record shortcut), lets it run,
# then closes the main window as a window manager's close button does
# (WM_DELETE_WINDOW). With no tray host the close must quit through
# Quit's path (`main.rs`, `tray_host.rs`), which saves the recording
# first. Exits 0 when the app logged that no tray host shows the icon,
# the close's quit and the shutdown's end, exited with 0 within 30 s, and
# the store holds the meeting `queued` with a duration above zero and
# `quit` as its end reason; 1 otherwise, 2 on a usage error or a missing
# tool.
#
#   scripts/pipewire-headless.sh \
#     apps/desktop/scripts/close-without-tray-linux.sh [path/to/steno-desktop] [seconds]
#
# `seconds` (default 4) is how long the recording runs before the close.
# Run it inside scripts/pipewire-headless.sh, which gives the recorder a
# microphone and an output to capture, and the app a private session bus
# with no watcher on it. Needs Xvfb, xdotool and python3 (to read the
# store and to send the close), and the web dist embedded as for the smoke
# (smoke-linux.sh). The app's log stays in the work directory it prints.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
binary="${1:-$root/target/debug/steno-desktop}"
seconds="${2:-4}"

[[ -x "$binary" ]] || { echo "close-without-tray: $binary is not an executable" >&2; exit 2; }
[[ "$seconds" =~ ^[1-9][0-9]*$ ]] \
  || { echo "close-without-tray: seconds must be a positive number, got \"$seconds\"" >&2; exit 2; }
for tool in Xvfb xdotool python3 ldd; do
  command -v "$tool" >/dev/null || { echo "close-without-tray: $tool is not on PATH" >&2; exit 2; }
done
# The X library the binary links, which sends the close.
libx11="$(ldd "$binary" | awk '/libX11\.so\.6/ { print $3; exit }')"
[[ -n "$libx11" ]] || { echo "close-without-tray: $binary does not link libX11" >&2; exit 2; }

work="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/steno-close-without-tray.XXXXXX")"
echo "close-without-tray: work directory $work"
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
  echo "close-without-tray: FAILED, $*" >&2
  echo "close-without-tray: the app's log:" >&2
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

# Sends WM_DELETE_WINDOW to the window $1, as a window manager does for
# its close button; Xvfb runs none.
close_window() {
  python3 - "$libx11" "$1" <<'EOF'
import ctypes, sys

x11 = ctypes.CDLL(sys.argv[1])
x11.XOpenDisplay.restype = ctypes.c_void_p
x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
x11.XInternAtom.restype = ctypes.c_ulong
x11.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
x11.XSendEvent.argtypes = [
    ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_long, ctypes.c_void_p,
]
x11.XFlush.argtypes = [ctypes.c_void_p]
x11.XCloseDisplay.argtypes = [ctypes.c_void_p]


class ClientMessage(ctypes.Structure):
    _fields_ = [
        ("type", ctypes.c_int),
        ("serial", ctypes.c_ulong),
        ("send_event", ctypes.c_int),
        ("display", ctypes.c_void_p),
        ("window", ctypes.c_ulong),
        ("message_type", ctypes.c_ulong),
        ("format", ctypes.c_int),
        ("data", ctypes.c_long * 5),
    ]


class Event(ctypes.Union):
    _fields_ = [("xclient", ClientMessage), ("pad", ctypes.c_long * 24)]


display = x11.XOpenDisplay(None)
if not display:
    sys.exit("no X display")
window = int(sys.argv[2])
event = Event()
event.xclient.type = 33  # ClientMessage
event.xclient.window = window
event.xclient.message_type = x11.XInternAtom(display, b"WM_PROTOCOLS", 0)
event.xclient.format = 32
event.xclient.data[0] = x11.XInternAtom(display, b"WM_DELETE_WINDOW", 0)
event.xclient.data[1] = 0  # CurrentTime
if not x11.XSendEvent(display, window, 0, 0, ctypes.byref(event)):
    sys.exit("the close was not sent")
x11.XFlush(display)
x11.XCloseDisplay(display)
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
echo "close-without-tray: session bus ${DBUS_SESSION_BUS_ADDRESS:-none}"

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

# As in lost-display-linux.sh: the shortcut again while the store holds
# no meeting at all, since a press after a recording started would stop it.
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
grep -qF "no tray host shows the tray icon" "$work/app.log" \
  || grep -qF "no session bus, so no tray host" "$work/app.log" \
  || fail "the app did not say that no tray host shows its icon"
echo "close-without-tray: recording; closing the main window in $seconds s"
sleep "$seconds"

close_window "$main" || fail "the close could not be sent"
for _ in $(seq 300); do
  kill -0 "$app" 2>/dev/null || break
  sleep 0.1
done
kill -0 "$app" 2>/dev/null && fail "the app was still running 30 s after its main window closed"
code=0
wait "$app" || code=$?
app=""
echo "close-without-tray: the app exited with $code"
[[ "$code" == 0 ]] || fail "the app exited with $code, not 0"

grep -qF "the main window closed with no tray" "$work/app.log" \
  || fail "the app did not quit for the close"
grep -qF "the shutdown ended" "$work/app.log" \
  || fail "the app did not log the shutdown's end"
rows="$(meetings "$store")"
echo "close-without-tray: the store holds: ${rows:-nothing}"
[[ -n "$rows" && "$(wc -l <<<"$rows")" == 1 ]] || fail "expected one meeting"
read -r state duration reason <<<"$rows"
[[ "$state" == queued ]] || fail "the meeting is $state, not queued"
[[ "$reason" == quit ]] || fail "the meeting ended with $reason, not quit"
python3 -c 'import sys; sys.exit(0 if float(sys.argv[1]) > 0 else 1)' "$duration" \
  || fail "the meeting has no duration ($duration)"
echo "close-without-tray: the app's lines on the tray and the exit:"
grep -E "tray host|main window closed|shutdown ended" "$work/app.log" | sed 's/^/  /'
echo "close-without-tray: ok, closing the main window quit and saved the recording"
