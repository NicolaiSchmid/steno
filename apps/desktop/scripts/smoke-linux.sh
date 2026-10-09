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
# unit's when the autostart entry is there, a reload asked for after a
# drop-in was written, and the owed reload's mark there exactly when that
# reload failed. A second launch in the same HOME then writes nothing,
# and asks for a reload only if one was still owed.
#
# Then runs as the autostart unit, in a throwaway HOME, inside a cgroup
# named after the unit below a delegated `systemd-run --user` scope, so
# `runs_as_autostart_unit` holds (autostart.rs, stop_timeout.rs). Each
# turns Launch at login off halfway (smoke.rs), which must leave the entry
# as it was and set the mark. The first is a first launch with
# ~/.config/autostart unwritable: it cannot restore the entry, must ask
# for no reload, and must leave the reload owed. A launch outside the unit
# then counts the first launch and writes the entry, which must name the
# binary's path whole; the script removes it and its drop-in. The next run
# as the unit must restore the entry, marked, with its drop-in, ask for
# the reload, turn Launch at login on again and off once more (smoke.rs),
# and remove the entry and its drop-in at the exit, after the shutdown's
# line. The last ends as an update's relaunch (STENO_SMOKE_RELAUNCH),
# which must keep the entry, the mark and the drop-in. Every launch names
# the binary in STENO_EXEC_PATH, since a build under target/ has no path
# that outlives an upgrade. Without a user manager that starts the scope,
# or as a root that can write to the directory the first run needs
# unwritable, these runs are skipped, unless STENO_REQUIRE_UNIT_SMOKE is
# set (CI), which fails instead.
#
# Then runs with the login item the system's (STENO_LOGIN_ITEM=managed,
# packaged.rs), each in a throwaway HOME holding an autostart entry an
# earlier build wrote, whose Exec starts a program in /nix/store. The
# first must remove it and leave the autostart unit's drop-in alone. The
# second runs as the autostart unit, as above: the entry must still be
# there halfway through the run, the unit must get its drop-in and the
# reload, and both must be gone after the exit, which removes them after
# the shutdown. The third, as the unit too, ends as an update's relaunch
# and must keep both. Without a user manager that starts the scope the
# runs as the unit are skipped, unless STENO_REQUIRE_UNIT_SMOKE is set
# (CI), which fails instead.
#
# Then it launches the binary once more over a database it cannot open,
# in a throwaway XDG_DATA_HOME and HOME, and expects the refusal: the
# "not starting" line and exit 3, and an autostart entry marked to go
# still there, since a refused launch changes no login item. It fails
# (exit 1) otherwise.
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
# The default filter, and the drop-ins' changes and reload.
export RUST_LOG=warn,steno_desktop::stop_timeout=debug
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
# The autostart entry under a HOME, and in a config home the mark that
# turns it off at the exit and the owed reload's (autostart.rs,
# stop_timeout.rs).
identifier="$(sed -n 's/^  "identifier": "\(.*\)",$/\1/p' "$root/apps/desktop/src-tauri/tauri.conf.json")"
[[ -n "$identifier" ]] || fail "no identifier in tauri.conf.json"
entry() { echo "$1/.config/autostart/steno-desktop.desktop"; }
mark() { echo "$1/$identifier/launch-at-login-off-at-exit"; }
owed() { echo "$1/$identifier/systemd-reload-owed"; }
reloads='the systemd user manager (reloaded|did not reload)'
# The autostart unit's drop-in under a HOME, and the unit as the log
# quotes it: unit="app-steno\\x2ddesktop@autostart.service".
autostart_drop_in() { echo "$1/.config/systemd/user/app-steno\x2ddesktop@autostart.service.d/10-steno.conf"; }
service='unit="app-steno[^"]*@autostart\.service"'
# Whether the log $1 shows the autostart unit's drop-in removed after the
# shutdown's line.
removed_after_shutdown() {
  local ended removed
  ended="$(grep -nF "the shutdown ended" "$1" | head -n1 | cut -d: -f1)"
  removed="$(grep -nE "drop-in changed $service on=false" "$1" | head -n1 | cut -d: -f1)"
  [[ -n "$ended" && -n "$removed" ]] && (( ended < removed ))
}

grep -qE 'WARN.*the shutdown ended' "$log" || fail "no warn line with the shutdown's duration"
units="$HOME/.config/systemd/user"
[[ -f "$units/app-gnome-steno\x2ddesktop-.scope.d/zz-steno.conf" ]] \
  || fail "no stop timeout drop-in for GNOME's scope under $units"
if [[ -f "$(entry "$HOME")" ]]; then
  [[ -f "$(autostart_drop_in "$HOME")" ]] \
    || fail "the autostart entry is there without its stop timeout drop-in"
fi
if grep -qE 'drop-in changed.*on.*true' "$log"; then
  grep -qE "$reloads" "$log" || fail "a drop-in was written and no reload asked for"
fi
config="${XDG_CONFIG_HOME:-$HOME/.config}"
# A reload that went through clears the mark, and one that failed leaves
# it, for the next launch to ask again.
if grep -qF 'the systemd user manager reloaded' "$log"; then
  [[ ! -e "$(owed "$config")" ]] || fail "a reload went through and stays owed"
fi
if grep -qF 'the systemd user manager did not reload' "$log"; then
  [[ -e "$(owed "$config")" ]] || fail "a reload failed and is not owed"
fi
echo "smoke: the shutdown's duration logged at warn, the stop timeout drop-ins in place"

# A second launch over what the first left writes no drop-in, and asks
# for the reload only while one is owed.
owed_before=false
[[ -e "$(owed "$config")" ]] && owed_before=true
xvfb-run --auto-servernum --server-args="$server_args" "$binary" > "$scratch/again" 2>&1 \
  || { cat "$scratch/again" >&2; fail "the second launch failed"; }
if grep -qF 'drop-in changed' "$scratch/again"; then
  cat "$scratch/again" >&2
  fail "the second launch wrote a drop-in again"
fi
if $owed_before && ! grep -qE "$reloads" "$scratch/again"; then
  cat "$scratch/again" >&2
  fail "a reload was owed and the second launch did not ask for it"
fi
if ! $owed_before && grep -qE "$reloads" "$scratch/again"; then
  cat "$scratch/again" >&2
  fail "nothing was written or owed and the second launch reloaded"
fi
echo "smoke: a second launch wrote nothing and reloaded only for an owed reload ($owed_before)"

# Runs "$@" in a cgroup named after the autostart unit, below a delegated
# scope of the user manager's.
as_unit() {
  systemd-run --user --scope -p Delegate=yes --quiet bash -c '
    cgroup="/sys/fs/cgroup$(sed -n "s/^0:://p" /proc/self/cgroup)/app-steno\x2ddesktop@autostart.service"
    mkdir "$cgroup" && echo 0 > "$cgroup/cgroup.procs" && exec "$@"
  ' _ "$@"
}

home="$scratch/unit"
mkdir -p "$home/.config/autostart"
chmod a-w "$home/.config/autostart"
unit_skip=""
if ! as_unit true 2>/dev/null; then
  unit_skip="no user manager started a delegated scope (systemd-run --user --scope)"
elif touch "$home/.config/autostart/probe" 2>/dev/null; then
  unit_skip="$(id -un) can write to a directory without write permission (root)"
fi
if [[ -z "$unit_skip" ]]; then
  # One run in $home; its output, both streams, in $1; $2 runs the binary
  # (as_unit, or empty). xvfb-run starts a new bash, so as_unit goes with
  # it as source.
  home_run() {
    HOME="$home" XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share" \
      XDG_CACHE_HOME="$home/.cache" \
      RUST_LOG="$RUST_LOG,steno_desktop::autostart=info" \
      xvfb-run --auto-servernum --server-args="$server_args" \
      bash -c "$(declare -f as_unit); $2 \"\$@\"" _ "$binary" > "$1" 2>&1 \
      || { cat "$1" >&2; fail "a run in $home failed (${2:-not} as the autostart unit)"; }
  }
  unit_run() { home_run "$1" as_unit; }
  unit_run "$scratch/unrestored"
  chmod u+w "$home/.config/autostart"
  for line in "the autostart entry could not be kept until the exit" \
    "no reload while the app runs as the autostart unit without its entry" \
    "Launch at login turned off waits for the exit (the entry is absent)"; do
    grep -qF "$line" "$scratch/unrestored" || { cat "$scratch/unrestored" >&2; fail "as the unit with no entry to restore: no \"$line\""; }
  done
  if grep -qE "$reloads" "$scratch/unrestored"; then
    fail "as the unit without its entry, a reload was asked for"
  fi
  [[ -e "$(owed "$home/.config")" ]] \
    || fail "as the unit without its entry, the skipped reload is not owed"
  # A launch outside the unit counts the first launch, so the next one
  # registers nothing, and writes the entry naming the path whole (the
  # plugin's own has a space after it). Without that entry the next launch
  # must restore it.
  home_run "$scratch/counted" ""
  grep -qxF "Exec=$binary" "$(entry "$home")" \
    || { cat "$(entry "$home")" >&2; fail "the entry written outside an AppImage does not name $binary"; }
  rm -f "$(entry "$home")"
  rm -f "$(autostart_drop_in "$home")"
  unit_run "$scratch/restored"
  if ! grep -qF "the autostart entry is back, marked to go at the exit" "$scratch/restored" \
    || ! grep -qF "Launch at login turned off waits for the exit (the entry stands)" "$scratch/restored" \
    || ! grep -qE "drop-in changed $service on=true" "$scratch/restored" \
    || ! grep -qE "$reloads" "$scratch/restored"; then
    cat "$scratch/restored" >&2
    fail "as the unit, the launch did not restore the entry and its drop-in, and reload"
  fi
  removed_after_shutdown "$scratch/restored" \
    || { cat "$scratch/restored" >&2; fail "the exit did not remove the drop-in after the shutdown"; }
  [[ ! -e "$(entry "$home")" && ! -e "$(mark "$home/.config")" ]] \
    || fail "the exit left the entry or its mark"
  # An update's relaunch runs on in the same unit: its exit keeps the
  # restored entry, the mark and the drop-in, for the next process.
  STENO_SMOKE_RELAUNCH=1 unit_run "$scratch/relaunched"
  if ! grep -qF "ending as an update's relaunch" "$scratch/relaunched" \
    || ! grep -qE "drop-in changed $service on=true" "$scratch/relaunched" \
    || grep -qE "drop-in changed $service on=false" "$scratch/relaunched" \
    || [[ ! -e "$(entry "$home")" || ! -e "$(mark "$home/.config")" || ! -f "$(autostart_drop_in "$home")" ]]; then
    cat "$scratch/relaunched" >&2
    fail "as the autostart unit, an update's relaunch did not keep the entry, its mark and its drop-in"
  fi
  echo "smoke: as the autostart unit, Launch at login turned off waited for the exit, on again kept the entry, the launch restored the entry only where it could, and an update's relaunch kept it"
elif [[ -n "${STENO_REQUIRE_UNIT_SMOKE:-}" ]]; then
  fail "$unit_skip, and STENO_REQUIRE_UNIT_SMOKE is set"
else
  chmod u+w "$home/.config/autostart"
  echo "smoke: $unit_skip; the runs as the autostart unit are skipped"
fi

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
    RUST_LOG="$RUST_LOG,steno_desktop::packaged=info" STENO_SMOKE_SECONDS=8 \
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
if [[ -e "$(entry "$scratch/managed")" ]] || grep -qE "drop-in changed $service" "$scratch/managed/output"; then
  cat "$scratch/managed/output" >&2
  fail "managed, the launch did not remove an earlier build's store entry, or changed the autostart unit's drop-in"
fi
echo "smoke: managed, an earlier build's store entry went at launch, and the autostart unit's drop-in was left alone"
if as_unit true 2>/dev/null; then
  managed_run "$scratch/managed-unit" as_unit
  output="$scratch/managed-unit/output"
  if [[ "$(cat "$scratch/managed-unit/halfway")" != there || -e "$(entry "$scratch/managed-unit")" ]] \
    || ! grep -qF "goes when Steno exits" "$output" \
    || ! grep -qF "removed the autostart entry an earlier build wrote" "$output" \
    || ! grep -qE "drop-in changed $service on=true" "$output" \
    || ! grep -qE "$reloads" "$output" \
    || ! removed_after_shutdown "$output" \
    || [[ -e "$(autostart_drop_in "$scratch/managed-unit")" ]]; then
    cat "$output" >&2
    fail "managed, as the autostart unit, the earlier build's entry and the unit's drop-in did not stay until the exit and go then"
  fi
  # An update's relaunch keeps both for the next process in the unit.
  STENO_SMOKE_RELAUNCH=1 managed_run "$scratch/managed-relaunched" as_unit
  output="$scratch/managed-relaunched/output"
  if [[ ! -e "$(entry "$scratch/managed-relaunched")" || ! -f "$(autostart_drop_in "$scratch/managed-relaunched")" ]] \
    || ! grep -qF "ending as an update's relaunch" "$output" \
    || grep -qF "removed the autostart entry an earlier build wrote" "$output" \
    || grep -qE "drop-in changed $service on=false" "$output"; then
    cat "$output" >&2
    fail "managed, as the autostart unit, an update's relaunch did not keep the earlier build's entry and the unit's drop-in"
  fi
  echo "smoke: managed, as the autostart unit, an earlier build's store entry and the unit's drop-in stayed while the app ran and went at the exit, and an update's relaunch kept both"
elif [[ -n "${STENO_REQUIRE_UNIT_SMOKE:-}" ]]; then
  fail "no user manager started a delegated scope (systemd-run --user --scope), and STENO_REQUIRE_UNIT_SMOKE is set"
else
  echo "smoke: no user manager started a delegated scope; the managed run as the autostart unit is skipped"
fi

# Then a launch over a database it cannot open, in a throwaway support
# directory: the shell must refuse with its dialog (`refuse_to_start`),
# which nobody closes here, and exit 3 once the smoke's wait ends, instead
# of panicking. Its HOME holds an entry marked to go, which a launch that
# went on would remove (`LaunchStep::TurnOff`).
refusal="$scratch/refusal"
mkdir -p "$refusal/Steno" "$(dirname "$(entry "$refusal")")" "$(dirname "$(mark "$refusal/.config")")"
printf 'not a database\n' > "$refusal/Steno/steno.sqlite"
touch "$(entry "$refusal")" "$(mark "$refusal/.config")"
# Both streams go to one file: Debian's xvfb-run sends the command's
# stderr to its stdout.
code=0
HOME="$refusal" XDG_CONFIG_HOME="$refusal/.config" XDG_DATA_HOME="$refusal" STENO_SMOKE_SECONDS=3 \
  xvfb-run --auto-servernum "$binary" > "$refusal/output" 2>&1 || code=$?
if [[ "$code" != 3 ]] || ! grep -qF "[steno-desktop] not starting:" "$refusal/output"; then
  cat "$refusal/output" >&2
  fail "over a database it cannot open the shell must refuse (exit 3), got $code"
fi
[[ -e "$(entry "$refusal")" && -e "$(mark "$refusal/.config")" ]] \
  || fail "the refused launch changed the login item"
echo "smoke: a database it cannot open is refused (exit 3), the login item untouched"
