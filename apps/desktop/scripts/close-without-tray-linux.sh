#!/usr/bin/env bash
# Closing the main window on Linux, with no tray and beside one: runs the
# built shell on an Xvfb server of its own with a fresh HOME, starts a
# recording from the main window (Ctrl+Shift+R, the record shortcut), lets
# it run, then closes the main window as a window manager's close button
# does (WM_DELETE_WINDOW).
#
# On a session bus where no status notifier watcher runs (stock GNOME's
# case) the close must quit through Quit's path (`main.rs`,
# `tray_host.rs`), which saves the recording first. Exits 0 when the app
# exited with 0 within 30 s, logged the close's quit, the shutdown's end
# and, with a session bus, that no tray host shows the icon, and the store
# holds the meeting `queued` with a duration above zero and `quit` as its
# end reason.
#
# With `--with-host`, beside a stand-in watcher with a host
# (tray-watcher-linux.py): the app must log that a host shows the icon, its
# tray's menu must hold every action in order, the close must hide the main
# window while the app goes on recording, and Quit Steno in the tray's
# menu must then quit and save as above.
#
# Exits 1 when a check fails, 2 on a usage error or a missing tool.
#
#   scripts/pipewire-headless.sh \
#     apps/desktop/scripts/close-without-tray-linux.sh [--with-host] [path/to/steno-desktop] [seconds]
#
# `seconds` (default 4) is how long the recording runs before the close.
# Run it inside scripts/pipewire-headless.sh, which gives the recorder a
# microphone and an output to capture, and the app a private session bus
# with no watcher on it. Needs Xvfb, xdotool and python3 (to read the
# store and to send the close; with `--with-host` also Python's GObject
# bindings), and the web dist embedded as for the smoke (smoke-linux.sh).
# The app's log stays in the work directory it prints. The steps it shares
# with lost-display-linux.sh are in xvfb-recording-linux.sh.
set -euo pipefail

name=close-without-tray
with_host=""
proved="closing the main window quit and saved the recording"
if [[ "${1:-}" == --with-host ]]; then
  name=close-beside-a-tray-host
  with_host=yes
  proved="the close hid the main window and Quit saved the recording"
  shift
fi
scripts="$(dirname "${BASH_SOURCE[0]}")"
# shellcheck source=apps/desktop/scripts/xvfb-recording-linux.sh
source "$scripts/xvfb-recording-linux.sh" "$@"
command -v ldd >/dev/null || { echo "$name: ldd is not on PATH" >&2; exit 2; }
# The X library the binary links, which sends the close. awk reads all of
# ldd's output: an early exit fails ldd's write, and the pipeline with it.
libx11="$(ldd "$binary" | awk '$1 == "libX11.so.6" { print $3 }')"
[[ -n "$libx11" ]] || { echo "$name: $binary does not link libX11" >&2; exit 2; }

# Sends WM_DELETE_WINDOW to the window $1, as a window manager does for
# its close button; Xvfb runs none. Not xdotool's `windowquit`: that asks
# a window manager (`_NET_CLOSE_WINDOW`), and Ubuntu 24.04's xdotool
# predates it.
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

# Waits 10 s at most for the fixed string $1 in the file $2.
await_line() {
  for _ in $(seq 100); do
    grep -qF "$1" "$2" && return 0
    sleep 0.1
  done
  return 1
}

echo "$name: session bus ${DBUS_SESSION_BUS_ADDRESS:-none}"
if [[ -z "$with_host" ]]; then
  start_recording
  echo "$name: recording; closing the main window in $seconds s"
  sleep "$seconds"

  close_window "$main" || fail "the close could not be sent"
  await_exit "its main window closed"
  [[ "$code" == 0 ]] || fail "the app exited with $code, not 0"

  if [[ -n "${DBUS_SESSION_BUS_ADDRESS:-}" ]]; then
    grep -qF "no tray host shows the tray icon" "$work/app.log" \
      || fail "the app did not say that no tray host shows its icon"
  else
    grep -qF "no session bus, so no tray host" "$work/app.log" \
      || fail "the app did not say that it has no session bus"
  fi
  grep -qF "the main window closed with no tray" "$work/app.log" \
    || fail "the app did not quit for the close"
else
  [[ -n "${DBUS_SESSION_BUS_ADDRESS:-}" ]] || { echo "$name: --with-host needs a session bus" >&2; exit 2; }
  python3 -c 'import gi' 2>/dev/null \
    || { echo "$name: Python's GObject bindings (python3-gi) are missing" >&2; exit 2; }
  python3 "$scripts/tray-watcher-linux.py" serve >"$work/watcher.log" 2>&1 &
  watcher=$!
  trap 'kill -KILL "$watcher" 2>/dev/null || true; cleanup' EXIT
  await_line "tray-watcher: ready" "$work/watcher.log" \
    || fail "the stand-in watcher did not start: $(cat "$work/watcher.log")"

  start_recording
  await_line "a tray host shows the tray icon" "$work/app.log" \
    || fail "the app did not say that a tray host shows its icon"
  await_line "tray-watcher: item " "$work/watcher.log" \
    || fail "the tray's item did not register with the watcher"
  read -r _ _ bus_name item_path < <(grep -F "tray-watcher: item " "$work/watcher.log" | head -n 1)

  # The rows while recording, as `tray::MENU` lays them out; the Record
  # row follows the recorder a moment after the store does.
  expected="$(printf '%s\n' "Stop recording" "Record in person (disabled)" - \
    "Open Steno" "Settings" - "Launch at login" "Check for Updates" - "Quit Steno")"
  rows=""
  for _ in $(seq 50); do
    rows="$(python3 "$scripts/tray-watcher-linux.py" menu "$bus_name" "$item_path")" \
      || fail "the tray's menu could not be read"
    [[ "$rows" == "$expected" ]] && break
    sleep 0.2
  done
  [[ "$rows" == "$expected" ]] || fail "the tray's menu holds:
$rows
expected:
$expected"
  echo "$name: the tray's menu holds every action"

  echo "$name: recording; closing the main window in $seconds s"
  sleep "$seconds"
  close_window "$main" || fail "the close could not be sent"
  sleep 3
  kill -0 "$app" 2>/dev/null || fail "the app ended although a tray host shows its icon"
  if xdotool search --onlyvisible --name '^Steno$' >/dev/null 2>&1; then
    fail "the main window still shows after its close"
  fi
  grep -q '^recording ' <<<"$(meetings "$store")" \
    || fail "the recording did not go on after the close: $(meetings "$store")"
  if grep -qF "the main window closed with no tray" "$work/app.log"; then
    fail "the close quit although a tray host shows the icon"
  fi
  echo "$name: the close hid the main window, and the recording goes on"

  python3 "$scripts/tray-watcher-linux.py" click "$bus_name" "$item_path" "Quit Steno" \
    || fail "Quit Steno could not be clicked"
  await_exit "Quit Steno in the tray's menu"
  [[ "$code" == 0 ]] || fail "the app exited with $code, not 0"
fi

grep -qF "the shutdown ended" "$work/app.log" \
  || fail "the app did not log the shutdown's end"
require_saved
[[ "$reason" == quit ]] || fail "the meeting ended with $reason, not quit"
echo "$name: the app's lines on the tray and the exit:"
grep -E "tray host|main window closed|shutdown ended" "$work/app.log" | sed 's/^/  /'
echo "$name: ok, $proved"
