#!/usr/bin/env bash
# Checks that the installer bundles carry `steno-speech-sidecar` beside
# `steno-desktop` once installed, where `SidecarConfig::beside_current_exe`
# (crates/steno-speech) looks for it. Each bundle is unpacked the way its
# installer lays it out, and the sidecar is started from there: it greets
# with its `ready` frame and exits when its stdin ends.
#
#   apps/desktop/scripts/check-bundle.sh [--signed] <bundle dir> <types>
#
# <types> is the comma-separated list Tauri's `--bundles` takes:
#
#   deb       deb/*.deb, unpacked with dpkg-deb -x, into usr/bin/; also
#             the stop timeout drop-ins for the autostart unit and GNOME's
#             scope under usr/lib/systemd/user/
#   appimage  appimage/*.AppImage, unpacked with --appimage-extract, into
#             usr/bin/
#   app       macos/*.app, into Contents/MacOS/. With --signed, also the
#             Developer ID signature, hardened runtime, timestamp and team
#             of the app, its executable and the sidecar, the two
#             entitlements, the stapled ticket and Gatekeeper's verdict
#   dmg       nothing of its own: the image holds the .app checked above
#   msi       msi/*.msi, unpacked by an administrative install; the install
#             directory also holds DirectML.dll and the Visual C++ runtime
#             (checked by name: the runner has system copies of both)
#   nsis      nsis/*-setup.exe, installed silently into a scratch directory;
#             the same as msi
#
# With STENO_HOST_PATHS set (one absolute path per line, e.g. the build
# host's workspace and cargo home), every type also checks that neither
# binary contains any of them, i.e. that the path remapping took effect.
#
# Exits non-zero at the first failed check (with an `::error::` for the
# script's own checks).
set -euo pipefail

signed=false
if [[ "${1:-}" == "--signed" ]]; then
  signed=true
  shift
fi
bundle="${1:?bundle directory, e.g. target/release/bundle}"
types="${2:?bundle types, e.g. deb,appimage}"
sidecar_name=steno-speech-sidecar
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

die() {
  echo "::error::$*" >&2
  exit 1
}

# The one file matching a glob, or die.
one() {
  local matches=()
  shopt -s nullglob
  # shellcheck disable=SC2206 # the argument is a glob on purpose
  matches=($1)
  shopt -u nullglob
  [[ ${#matches[@]} -eq 1 ]] || die "expected one $1, found ${#matches[@]}"
  echo "${matches[0]}"
}

# no_host_paths <file...>: none of the files contains a line of
# STENO_HOST_PATHS. Nothing to check when it is unset.
no_host_paths() {
  local file path
  for file in "$@"; do
    while IFS= read -r path; do
      [[ -n "$path" ]] || continue
      if grep -qaF -- "$path" "$file"; then
        die "$file contains the build host's path $path"
      fi
    done <<< "${STENO_HOST_PATHS:-}"
  done
  [[ -z "${STENO_HOST_PATHS:-}" ]] || echo "ok: no build host path in $*"
}

# side_by_side <dir> <app binary> <sidecar>: both are files in <dir>, the
# sidecar greets and exits on an empty stdin, and neither holds a host path.
side_by_side() {
  local dir="$1" app="$2" sidecar="$3" greeting
  [[ -f "$dir/$app" ]] || die "$app is not in $dir"
  [[ -f "$dir/$sidecar" ]] || die "$sidecar is not beside $app in $dir"
  greeting="$("$dir/$sidecar" < /dev/null | tr -d '\0' || true)"
  [[ "$greeting" == *'"type":"ready"'* ]] || die "$dir/$sidecar did not greet: $greeting"
  echo "ok: $dir holds $app and $sidecar, which runs"
  no_host_paths "$dir/$app" "$dir/$sidecar"
}

# drop_in <path below usr/lib/systemd/user/> <file in src-tauri/linux/>:
# the unpacked .deb holds that file there, byte for byte.
drop_in() {
  local target="usr/lib/systemd/user/$1"
  cmp -s "$scratch/deb/$target" "$root/apps/desktop/src-tauri/linux/$2" \
    || die "the .deb does not install linux/$2 as /$target"
  echo "ok: the .deb installs /$target"
}

check_deb() {
  local deb
  deb="$(one "$bundle/deb/*.deb")"
  dpkg-deb -x "$deb" "$scratch/deb"
  side_by_side "$scratch/deb/usr/bin" steno-desktop "$sidecar_name"
  # The stop timeout drop-ins for the unit systemd makes from the autostart
  # entry and for GNOME's scope (apps/desktop/src-tauri/src/stop_timeout.rs);
  # `\x2d` is literal, as systemd names the directories.
  drop_in 'app-steno\x2ddesktop@autostart.service.d/10-steno.conf' autostart-service-stop-timeout.conf
  drop_in 'app-gnome-steno\x2ddesktop-.scope.d/zz-steno.conf' gnome-scope-stop-timeout.conf
}

check_appimage() {
  local appimage
  appimage="$(one "$bundle/appimage/*.AppImage")"
  appimage="$(cd "$(dirname "$appimage")" && pwd)/${appimage##*/}"
  mkdir -p "$scratch/appimage"
  (cd "$scratch/appimage" && "$appimage" --appimage-extract >/dev/null)
  side_by_side "$scratch/appimage/squashfs-root/usr/bin" steno-desktop "$sidecar_name"
}

# The `codesign -dv` details of one item after the release checks: the
# Developer ID authority, the hardened runtime flag and a secure
# timestamp. Prints the team.
release_signature() {
  local item="$1" details
  details="$(codesign -dv --verbose=4 "$item" 2>&1)" || die "codesign -dv failed on $item: $details"
  grep -q '^Authority=Developer ID Application' <<< "$details" \
    || die "$item is not signed with a Developer ID Application certificate: $details"
  grep -Eq '^(CodeDirectory|flags=).*runtime' <<< "$details" \
    || die "$item lacks the hardened runtime flag: $details"
  grep -q '^Timestamp=' <<< "$details" || die "$item carries no secure timestamp: $details"
  sed -n 's/^TeamIdentifier=//p' <<< "$details"
}

check_app() {
  local app executable team item item_team entitlements count
  app="$(one "$bundle/macos/*.app")"
  executable="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$app/Contents/Info.plist")"
  side_by_side "$app/Contents/MacOS" "$executable" "$sidecar_name"
  [[ "$signed" == true ]] || return 0

  codesign --verify --deep --strict --verbose=2 "$app"
  team="$(release_signature "$app")"
  [[ -n "$team" ]] || die "no TeamIdentifier on $app"
  for item in "$app/Contents/MacOS/$executable" "$app/Contents/MacOS/$sidecar_name"; do
    item_team="$(release_signature "$item")"
    [[ "$item_team" == "$team" ]] || die "$item is signed by team '$item_team', expected '$team'"
  done
  entitlements="$(codesign -d --entitlements - --xml "$app" 2>/dev/null)"
  count="$(grep -o '<key>' <<< "$entitlements" | wc -l | tr -d ' ')"
  grep -q 'com.apple.security.device.audio-input' <<< "$entitlements" || die "audio-input entitlement missing: $entitlements"
  grep -q 'com.apple.security.personal-information.calendars' <<< "$entitlements" || die "calendars entitlement missing: $entitlements"
  [[ "$count" == 2 ]] || die "expected exactly two entitlements on $app, found $count: $entitlements"
  xcrun stapler validate "$app"
  spctl --assess --type execute --verbose=2 "$app"
  echo "ok: $app is signed by team $team, notarised and stapled"
}

# windows_wait <program> <argument...>: runs a Windows program through
# PowerShell and waits for it, since msiexec and an NSIS installer return
# to Git Bash before they finish. Arguments are passed as they are; none
# may contain a single quote.
windows_wait() {
  local program="$1" arguments="" argument
  shift
  for argument in "$@"; do
    arguments+="${arguments:+, }'$argument'"
  done
  # Git Bash would rewrite the slashes of /a, /qn and /S as paths.
  MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*' powershell.exe -NoProfile -Command \
    "\$p = Start-Process -FilePath '$program' -Wait -PassThru -ArgumentList $arguments; exit \$p.ExitCode"
}

# windows_install <tree> <installer>: the directory under <tree> the
# sidecar landed in holds the app, the libraries both load, and a sidecar
# that runs.
windows_install() {
  local dir library
  dir="$(find "$1" -name "$sidecar_name.exe" -exec dirname {} \; | head -n 1)"
  [[ -n "$dir" ]] || die "$sidecar_name.exe is not among the files $2 installs"
  for library in DirectML.dll msvcp140.dll; do
    [[ -n "$(find "$dir" -maxdepth 1 -iname "$library")" ]] || die "$library is not beside the binaries $2 installs in $dir"
  done
  side_by_side "$dir" steno-desktop.exe "$sidecar_name.exe"
}

check_msi() {
  local msi
  msi="$(one "$bundle/msi/*.msi")"
  # An administrative install unpacks the files into the directory tree the
  # installer would create, without registering anything.
  windows_wait msiexec.exe /a "\"$(cygpath -aw "$msi")\"" /qn "TARGETDIR=\"$(cygpath -aw "$scratch/msi")\""
  windows_install "$scratch/msi" "$msi"
}

check_nsis() {
  local exe
  exe="$(one "$bundle/nsis/*-setup.exe")"
  # A silent install for the current user into a scratch directory (`/D`
  # takes the rest of the command line, unquoted, so it comes last).
  windows_wait "$(cygpath -aw "$exe")" /S "/D=$(cygpath -aw "$scratch/nsis")"
  windows_install "$scratch/nsis" "$exe"
}

for type in ${types//,/ }; do
  case "$type" in
    deb) check_deb ;;
    appimage) check_appimage ;;
    app) check_app ;;
    dmg) ;;
    msi) check_msi ;;
    nsis) check_nsis ;;
    *) die "unknown bundle type \"$type\" (known: deb, appimage, app, dmg, msi, nsis)" ;;
  esac
done
