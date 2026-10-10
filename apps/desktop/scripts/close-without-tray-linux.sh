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
# The steps it shares with lost-display-linux.sh are in
# xvfb-recording-linux.sh.
set -euo pipefail

name=close-without-tray
# shellcheck source=apps/desktop/scripts/xvfb-recording-linux.sh
source "$(dirname "${BASH_SOURCE[0]}")/xvfb-recording-linux.sh" "$@"
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

echo "close-without-tray: session bus ${DBUS_SESSION_BUS_ADDRESS:-none}"
start_recording
grep -qF "no tray host shows the tray icon" "$work/app.log" \
  || grep -qF "no session bus, so no tray host" "$work/app.log" \
  || fail "the app did not say that no tray host shows its icon"
echo "close-without-tray: recording; closing the main window in $seconds s"
sleep "$seconds"

close_window "$main" || fail "the close could not be sent"
await_exit "its main window closed"
[[ "$code" == 0 ]] || fail "the app exited with $code, not 0"

grep -qF "the main window closed with no tray" "$work/app.log" \
  || fail "the app did not quit for the close"
grep -qF "the shutdown ended" "$work/app.log" \
  || fail "the app did not log the shutdown's end"
require_saved
[[ "$reason" == quit ]] || fail "the meeting ended with $reason, not quit"
echo "close-without-tray: the app's lines on the tray and the exit:"
grep -E "tray host|main window closed|shutdown ended" "$work/app.log" | sed 's/^/  /'
echo "close-without-tray: ok, closing the main window quit and saved the recording"
