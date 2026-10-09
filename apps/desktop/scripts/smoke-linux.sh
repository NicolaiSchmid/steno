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
# corners render black there. After the run it checks the app's log and
# the stop timeout drop-ins (stop_timeout.rs): the shutdown's duration at
# warn, GNOME's scope drop-in in ~/.config/systemd/user, the autostart
# unit's when the autostart entry is there, and a reload asked for after
# a drop-in was written.
#
# Then two runs with the login item the system's (STENO_LOGIN_ITEM=managed,
# packaged.rs), each in a throwaway HOME holding an autostart entry an
# earlier build wrote, whose Exec starts a program in /nix/store. The
# first must remove it. The second runs as the autostart unit, inside a
# cgroup named after the unit below a delegated `systemd-run --user`
# scope: the entry must still be there halfway through the run, and gone
# after the exit, which removes it after the shutdown. Without a user manager
# that starts the scope the second run is skipped, unless
# STENO_REQUIRE_UNIT_SMOKE is set (CI), which fails instead.
#
# Then it launches the binary once more over a database it cannot open,
# in a throwaway XDG_DATA_HOME, and expects the refusal: the "not
# starting" line and exit 3, and fails (exit 1) otherwise.
#
#   [STENO_SMOKE_DPI=<dpi>] [STENO_REQUIRE_UNIT_SMOKE=1] apps/desktop/scripts/smoke-linux.sh [path/to/steno-desktop] [seconds]
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
# The default filter, and the drop-ins' changes and reload.
export RUST_LOG=warn,steno_desktop::stop_timeout=debug
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
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
  if command -v import >/dev/null; then
    sleep "$(( $2 > 3 ? $2 - 3 : 1 ))"
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
  exit "$status"
' _ "$binary" "$seconds" "$screens" "$log" || status=$?
(( status == 0 )) || exit "$status"

fail() { echo "smoke: $*" >&2; exit 1; }
grep -qE 'WARN.*the shutdown ended' "$log" || fail "no warn line with the shutdown's duration"
units="$HOME/.config/systemd/user"
[[ -f "$units/app-gnome-steno\x2ddesktop-.scope.d/zz-steno.conf" ]] \
  || fail "no stop timeout drop-in for GNOME's scope under $units"
if [[ -f "$HOME/.config/autostart/steno-desktop.desktop" ]]; then
  [[ -f "$units/app-steno\x2ddesktop@autostart.service.d/10-steno.conf" ]] \
    || fail "the autostart entry is there without its stop timeout drop-in"
fi
if grep -qE 'drop-in changed.*on.*true' "$log"; then
  grep -qE 'the systemd user manager (reloaded|did not reload)' "$log" \
    || fail "a drop-in was written and no reload asked for"
fi
echo "smoke: the shutdown's duration logged at warn, the stop timeout drop-ins in place"

entry() { echo "$1/.config/autostart/steno-desktop.desktop"; }

# Runs "$@" in a cgroup named after the autostart unit, below a delegated
# scope of the user manager's.
as_unit() {
  systemd-run --user --scope -p Delegate=yes --quiet bash -c '
    cgroup="/sys/fs/cgroup$(sed -n "s/^0:://p" /proc/self/cgroup)/app-steno\x2ddesktop@autostart.service"
    mkdir "$cgroup" && echo 0 > "$cgroup/cgroup.procs" && exec "$@"
  ' _ "$@"
}

# One managed run in the throwaway HOME $1, with an earlier build's entry,
# its output, both streams, in $1/output; $2 runs the binary (as_unit, or
# empty). Halfway through, it notes in $1/halfway whether the entry is
# still there.
managed_run() {
  mkdir -p "$(dirname "$(entry "$1")")"
  printf '[Desktop Entry]\nType=Application\nName=steno-desktop\nExec=/nix/store/aaaa-steno-desktop/bin/.steno-desktop-wrapped \n' \
    > "$(entry "$1")"
  HOME="$1" XDG_CONFIG_HOME="$1/.config" XDG_DATA_HOME="$1/.local/share" \
    XDG_CACHE_HOME="$1/.cache" STENO_LOGIN_ITEM=managed \
    RUST_LOG=warn,steno_desktop::packaged=info STENO_SMOKE_SECONDS=8 \
    xvfb-run --auto-servernum --server-args="$server_args" bash -c "
      $(declare -f as_unit)
      $2 \"\$1\" & app=\$!
      sleep 4
      [[ -e \"\$3\" ]] && echo there > \"\$4\" || echo gone > \"\$4\"
      wait \"\$app\"
    " _ "$binary" "$2" "$(entry "$1")" "$1/halfway" > "$1/output" 2>&1 \
    || { cat "$1/output" >&2; fail "a managed run failed"; }
}

managed_run "$scratch/managed" ""
[[ ! -e "$(entry "$scratch/managed")" ]] \
  || { cat "$scratch/managed/output" >&2; fail "managed, the launch did not remove an earlier build's store entry"; }
echo "smoke: managed, an earlier build's store entry went at launch"
if as_unit true 2>/dev/null; then
  managed_run "$scratch/unit" as_unit
  if [[ "$(cat "$scratch/unit/halfway")" != there || -e "$(entry "$scratch/unit")" ]] \
    || ! grep -qF "goes when Steno exits" "$scratch/unit/output" \
    || ! grep -qF "removed the autostart entry an earlier build wrote" "$scratch/unit/output"; then
    cat "$scratch/unit/output" >&2
    fail "managed, as the autostart unit, the earlier build's entry did not stay until the exit and go then"
  fi
  echo "smoke: managed, as the autostart unit, an earlier build's store entry stayed while the app ran and went at the exit"
elif [[ -n "${STENO_REQUIRE_UNIT_SMOKE:-}" ]]; then
  fail "no user manager started a delegated scope (systemd-run --user --scope), and STENO_REQUIRE_UNIT_SMOKE is set"
else
  echo "smoke: no user manager started a delegated scope; the managed run as the autostart unit is skipped"
fi

# Then a launch over a database it cannot open, in a throwaway support
# directory: the shell must refuse with its dialog (`refuse_to_start`),
# which nobody closes here, and exit 3 once the smoke's wait ends, instead
# of panicking.
refusal="$scratch/refusal"
mkdir -p "$refusal/Steno"
printf 'not a database\n' > "$refusal/Steno/steno.sqlite"
# Both streams go to one file: Debian's xvfb-run sends the command's
# stderr to its stdout.
code=0
XDG_DATA_HOME="$refusal" STENO_SMOKE_SECONDS=3 \
  xvfb-run --auto-servernum "$binary" > "$refusal/output" 2>&1 || code=$?
if [[ "$code" != 3 ]] || ! grep -qF "[steno-desktop] not starting:" "$refusal/output"; then
  cat "$refusal/output" >&2
  echo "smoke: over a database it cannot open the shell must refuse (exit 3), got $code" >&2
  exit 1
fi
echo "smoke: a database it cannot open is refused (exit 3)"
