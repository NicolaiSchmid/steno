#!/usr/bin/env bash
# The lost display on Linux: runs the built shell on an Xvfb server of its
# own with a fresh HOME, starts a recording from the main window
# (Ctrl+Shift+R, the record shortcut), lets it run, then ends the Xvfb
# server by the PID this script recorded. GDK ends the process when its X
# server goes; the shell's log writer (`display_lost.rs`) must save the
# recording first. Exits 0 when the app logged its save ("the display
# closed; saving") and the store holds the meeting `queued` with a
# duration above zero, 1 otherwise, 2 on a usage error or a missing tool.
#
#   scripts/pipewire-headless.sh \
#     apps/desktop/scripts/lost-display-linux.sh [path/to/steno-desktop] [seconds]
#
# `seconds` (default 4) is how long the recording runs before the server
# ends. Run it inside scripts/pipewire-headless.sh, which gives the
# recorder a microphone and an output to capture. Needs Xvfb, xdotool and
# python3 (to read the store), and the web dist embedded as for the smoke
# (smoke-linux.sh). The app's log stays in the work directory it prints.
# The steps it shares with close-without-tray-linux.sh are in
# xvfb-recording-linux.sh.
set -euo pipefail

name=lost-display
# shellcheck source=apps/desktop/scripts/xvfb-recording-linux.sh
source "$(dirname "${BASH_SOURCE[0]}")/xvfb-recording-linux.sh" "$@"

start_recording
echo "$name: recording; ending the display server in $seconds s"
sleep "$seconds"

kill -TERM "$server"
await_exit "the display server ended"

grep -qF "the display closed; saving" "$work/app.log" \
  || fail "the app did not save for the lost display"
require_saved
echo "$name: ok, the recording was saved before the app ended"
