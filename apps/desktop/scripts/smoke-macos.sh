#!/usr/bin/env bash
# The smoke of the Tauri shell on macOS: runs the built binary with
# STENO_SMOKE_SECONDS in the logged-in session, whose screen shows the
# three windows and both panels for that long, and exits as the run does
# (0 once the main window has sent page.ready, a snapshot reached it, the
# tray was built and the panels showed and hid; 1 when any did not). HOME
# is a fresh directory, so the run neither reads nor writes the user's
# Steno data or saved panel anchor.
#
# The single-instance socket is one per machine (/tmp, named after the
# bundle identifier), so a Steno shell already running takes the launch and
# the binary exits 0 at once: the run counts only with its own `smoke: ok`
# line, and runs on one machine (CI runners sharing a host) take turns
# through a lock.
#
#   apps/desktop/scripts/smoke-macos.sh [path/to/steno-desktop] [seconds]
#
# Needs the web dist embedded: pnpm build in apps/macos/web, then cargo
# build with TAURI_CONFIG='{"build":{"devUrl":null}}', as CI does.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
binary="${1:-$root/target/debug/steno-desktop}"
seconds="${2:-${STENO_SMOKE_SECONDS:-12}}"

[[ -x "$binary" ]] || { echo "smoke: $binary is not an executable" >&2; exit 2; }
[[ "$seconds" =~ ^[1-9][0-9]*$ ]] \
  || { echo "smoke: seconds must be a positive number, got \"$seconds\"" >&2; exit 2; }

home="$(mktemp -d "${TMPDIR:-/tmp}/steno-smoke.XXXXXX")"
trap 'rm -rf "$home"' EXIT
log="$home/smoke.log"

# Waits up to five minutes for another run to finish.
status=0
HOME="$home" STENO_SMOKE_SECONDS="$seconds" \
  lockf -t 300 /tmp/steno-desktop-smoke.lock "$binary" 2>&1 | tee "$log" || status=$?
(( status == 0 )) || exit "$status"
grep -q "smoke: ok" "$log" \
  || { echo "smoke: the run gave no verdict; a running Steno shell took its launch" >&2; exit 1; }
