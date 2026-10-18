#!/usr/bin/env bash
# Headless smoke of the Tauri shell on Linux: runs the built binary under
# xvfb-run with STENO_SMOKE_SECONDS, which makes the shell open all three
# windows side by side and both floating panels under Settings, and exit 0
# once the main window has sent page.ready, a snapshot reached it, the tray
# was built and the panels showed and hid (1 when any did not). When
# ImageMagick's `import` is available, the Xvfb root is captured near the
# end of the wait into apps/desktop/screens/ as review evidence, with one
# crop per window and per panel (`magick` from ImageMagick 7, `convert`
# from 6); the windows carry what the host's database holds (nothing on
# a fresh runner). Xvfb has no compositor, so the panels' transparent
# corners render black there. At the same moment, with xdotool on PATH,
# each panel must be a visible X11 window with the title a window
# manager's rules match ("Steno bubble", "Steno prompt", panels.rs), or
# the smoke fails; without xdotool that check is skipped. After the run
# the app's log must hold the shutdown's duration at warn.
#
# Then a launch in a throwaway HOME turns Launch at login on through the
# first launch's registration, and the entry must name the binary's path
# whole (STENO_EXEC_PATH, since a build under target/ has no path that
# outlives an upgrade). A launch with the login item the system's
# (STENO_LOGIN_ITEM=managed, packaged.rs), in a throwaway HOME holding an
# autostart entry an earlier build wrote, whose Exec starts a program in
# /nix/store, must remove that entry.
#
# Then it launches the binary once more over a database it cannot open,
# in a throwaway XDG_DATA_HOME and HOME, and expects the refusal: the
# "not starting" line and exit 3, and the autostart entry still there,
# since a refused launch changes no login item. A last launch, while
# another process holds the database's lock, expects the same exit. Both
# "not starting" lines must name no path, since the error-level log gets
# the same words (the `steno_services::logs` rule). It fails (exit 1)
# otherwise.
#
#   [STENO_SMOKE_DPI=<dpi>] apps/desktop/scripts/smoke-linux.sh [path/to/steno-desktop] [seconds]
#
# Needs the web dist embedded (pnpm build in apps/macos/web before cargo
# build) and the runtime libraries the binary links; on NixOS run it inside
# the nix-shell apps/desktop/README.md names, with xvfb-run and imagemagick.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
binary="${1:-$root/target/debug/steno-desktop}"
seconds="${2:-${STENO_SMOKE_SECONDS:-12}}"
screens="$root/apps/desktop/screens"

[[ -x "$binary" ]] || { echo "smoke: $binary is not an executable" >&2; exit 2; }
# The autostart entry names an absolute path (packaged/linux.rs), and CI
# passes a relative one.
binary="$(realpath -- "$binary")"
[[ "$seconds" =~ ^[1-9][0-9]*$ ]] \
  || { echo "smoke: seconds must be a positive number, got \"$seconds\"" >&2; exit 2; }
command -v xvfb-run >/dev/null || { echo "smoke: xvfb-run is not on PATH" >&2; exit 2; }

mkdir -p "$screens"
export STENO_SMOKE_SECONDS="$seconds"
# Software rendering: Xvfb has no GPU and WebKitGTK's DMA-BUF path fails
# without one.
export WEBKIT_DISABLE_DMABUF_RENDERER=1
export WEBKIT_DISABLE_COMPOSITING_MODE=1
export LIBGL_ALWAYS_SOFTWARE=1
export GDK_BACKEND=x11
# The path the autostart entry names: a build under target/ has none that
# outlives an upgrade, so turning Launch at login on would fail.
export STENO_EXEC_PATH="$binary"
export RUST_LOG=warn
scratch="$(mktemp -d)"
trap 'chmod -R u+w "$scratch"; rm -rf "$scratch"' EXIT
log="$scratch/log"

# 1120x720 main at the origin, Settings to its right, onboarding below.
# STENO_SMOKE_DPI sets the X resolution (Xvfb's own default otherwise);
# WebKitGTK's devicePixelRatio follows it, so 120 checks the panels at a
# ratio of 1.25.
server_args="-screen 0 2200x1500x24${STENO_SMOKE_DPI:+ -dpi $STENO_SMOKE_DPI}"
status=0
xvfb-run --auto-servernum --server-args="$server_args" bash -c '
  set -u
  # The log goes to $4 for the checks below, and to stderr as it comes.
  "$1" 2>"$4" & app=$!
  tail -f --pid="$app" "$4" >&2 &
  sleep "$(( $2 > 3 ? $2 - 3 : 1 ))"
  titles=0
  if command -v xdotool >/dev/null; then
    for title in "Steno bubble" "Steno prompt"; do
      if xdotool search --onlyvisible --name "^$title\$" >/dev/null; then
        echo "smoke: a visible window is titled \"$title\""
      else
        echo "smoke: no visible window is titled \"$title\"" >&2
        titles=1
      fi
    done
  else
    echo "smoke: xdotool is not on PATH; the panel titles are not checked"
  fi
  if command -v import >/dev/null; then
    import -window root "$3/smoke-root.png" && echo "smoke: captured $3/smoke-root.png"
    # One file per window, at the positions smoke.rs lays them out.
    # ImageMagick 7 is `magick`; 6 (the hosted Ubuntu image) is `convert`.
    crop=""
    if command -v magick >/dev/null; then crop=magick
    elif command -v convert >/dev/null; then crop=convert
    fi
    if [[ -n "$crop" ]]; then
      "$crop" "$3/smoke-root.png" -crop 1120x720+0+0 +repage "$3/main.png"
      "$crop" "$3/smoke-root.png" -crop 960x640+1160+0 +repage "$3/settings.png"
      "$crop" "$3/smoke-root.png" -crop 560x620+0+780 +repage "$3/onboarding.png"
      # The panels, where smoke.rs puts them (PANELS_X, PANEL_*_Y), with a
      # margin around each so the crop survives the page resizing the
      # window about its top centre, at 120 dpi too.
      "$crop" "$3/smoke-root.png" -crop 720x100+1040+680 +repage "$3/prompt.png"
      "$crop" "$3/smoke-root.png" -crop 720x100+1040+780 +repage "$3/bubble.png"
      echo "smoke: cropped main, settings, onboarding, prompt and bubble with $crop"
    fi
  fi
  wait "$app"; status=$?
  wait
  (( status == 0 )) || exit "$status"
  exit "$titles"
' _ "$binary" "$seconds" "$screens" "$log" || status=$?
(( status == 0 )) || exit "$status"

fail() { echo "smoke: $*" >&2; exit 1; }
# The autostart entry under a HOME.
entry() { echo "$1/.config/autostart/steno-desktop.desktop"; }

grep -qE 'WARN.*the shutdown ended' "$log" || fail "no warn line with the shutdown's duration"
echo "smoke: the shutdown's duration logged at warn"

# One run in the throwaway HOME $1, its output, both streams, in
# $1/output; the rest of the arguments go to env.
home_run() {
  mkdir -p "$1"
  env HOME="$1" XDG_CONFIG_HOME="$1/.config" XDG_DATA_HOME="$1/.local/share" \
    XDG_CACHE_HOME="$1/.cache" RUST_LOG="$RUST_LOG,steno_desktop::packaged=info" \
    "${@:2}" xvfb-run --auto-servernum --server-args="$server_args" "$binary" \
    > "$1/output" 2>&1 || { cat "$1/output" >&2; fail "a run in $1 failed"; }
}

# The first launch registers the login item, naming the path whole (the
# plugin's own has a space after it).
home_run "$scratch/first"
grep -qxF "Exec=$binary" "$(entry "$scratch/first")" \
  || { cat "$(entry "$scratch/first")" >&2; fail "the entry written outside an AppImage does not name $binary"; }
echo "smoke: the first launch wrote the autostart entry naming $binary"

managed="$scratch/managed"
mkdir -p "$(dirname "$(entry "$managed")")"
printf '[Desktop Entry]\nType=Application\nName=steno-desktop\nExec=/nix/store/aaaa-steno-desktop/bin/.steno-desktop-wrapped \n' \
  > "$(entry "$managed")"
home_run "$managed" STENO_LOGIN_ITEM=managed
if [[ -e "$(entry "$managed")" ]]; then
  cat "$managed/output" >&2
  fail "managed, the launch did not remove an earlier build's store entry"
fi
echo "smoke: managed, an earlier build's store entry went at launch"

# Then a launch over a database it cannot open, in a throwaway support
# directory: the shell must refuse with its dialog (`refuse_to_start`),
# which nobody closes here, and exit 3 once the smoke's wait ends, instead
# of panicking. Its HOME holds an autostart entry, which the refusal
# must leave.
refusal="$scratch/refusal"
mkdir -p "$refusal/Steno" "$(dirname "$(entry "$refusal")")"
printf 'not a database\n' > "$refusal/Steno/steno.sqlite"
touch "$(entry "$refusal")"
# Both streams go to one file: Debian's xvfb-run sends the command's
# stderr to its stdout.
code=0
HOME="$refusal" XDG_CONFIG_HOME="$refusal/.config" XDG_DATA_HOME="$refusal" STENO_SMOKE_SECONDS=3 \
  xvfb-run --auto-servernum "$binary" > "$refusal/output" 2>&1 || code=$?
line="$(grep -F "[steno-desktop] not starting:" "$refusal/output" || true)"
if [[ "$code" != 3 \
  || "$line" != "[steno-desktop] not starting: the database could not be opened or read (SQLite: NotADatabase)" ]]; then
  cat "$refusal/output" >&2
  fail "over a database it cannot open the shell must refuse (exit 3) with a line naming no path, got $code"
fi
[[ -e "$(entry "$refusal")" ]] \
  || fail "the refused launch changed the login item"
echo "smoke: a database it cannot open is refused (exit 3), the login item untouched"

# Then a launch while another process holds the database's lock, which
# flock takes on the throwaway support directory's steno.lock: the shell
# must refuse ("Steno is already running") and exit 3, and its line must
# not carry the lock's path, which the full error does.
held="$scratch/held"
mkdir -p "$held/Steno"
code=0
flock -x "$held/Steno/steno.lock" \
  env HOME="$held" XDG_CONFIG_HOME="$held/.config" XDG_DATA_HOME="$held" STENO_SMOKE_SECONDS=3 \
  xvfb-run --auto-servernum "$binary" > "$held/output" 2>&1 || code=$?
line="$(grep -F "[steno-desktop] not starting:" "$held/output" || true)"
if [[ "$code" != 3 || "$line" != "[steno-desktop] not starting: another Steno holds the database" ]]; then
  cat "$held/output" >&2
  fail "with the database's lock held the shell must refuse (exit 3) with a line naming no path, got $code"
fi
echo "smoke: a database another process holds is refused (exit 3), its line naming no path"
