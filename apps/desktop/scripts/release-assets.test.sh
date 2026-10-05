#!/usr/bin/env bash
# Checks apps/desktop/scripts/release-assets.sh, which gathers the desktop
# release's assets from the bundle jobs' artifacts; rust-ci.yml runs it on
# Linux.
set -euo pipefail

script="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/release-assets.sh"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
failures=0

fail() {
  echo "FAIL: $1"
  failures=$((failures + 1))
}

# dist <name> <platform...>: the artifacts of those platforms as
# actions/download-artifact leaves them without merge-multiple.
dist() {
  local dir="$scratch/$1" platform file files
  shift
  for platform in "$@"; do
    case "$platform" in
      linux) files=(deb/steno-desktop_0.2.0_amd64.deb deb/steno-desktop_0.2.0_amd64.deb.sig
        appimage/steno-desktop_0.2.0_amd64.AppImage appimage/steno-desktop_0.2.0_amd64.AppImage.sig) ;;
      windows) files=(msi/Steno_0.2.0_x64_en-US.msi msi/Steno_0.2.0_x64_en-US.msi.sig
        nsis/Steno_0.2.0_x64-setup.exe nsis/Steno_0.2.0_x64-setup.exe.sig) ;;
      macos) files=(dmg/Steno_0.2.0_aarch64.dmg macos/Steno.app.tar.gz macos/Steno.app.tar.gz.sig) ;;
    esac
    for file in "${files[@]}"; do
      mkdir -p "$dir/steno-desktop-$platform/${file%/*}"
      printf 'bytes of %s\n' "$file" > "$dir/steno-desktop-$platform/$file"
    done
  done
  echo "$dir"
}

# listing <dir>: its files, sorted, comma-separated.
listing() {
  find "$1" -mindepth 1 -printf '%f\n' | LC_ALL=C sort | paste -sd, -
}

# All three platforms.
dir="$(dist all linux windows macos)"
if output="$("$script" 0.2.0 "$dir" "$scratch/all-assets" linux windows macos 2>&1)"; then
  wanted='Steno_0.2.0_aarch64.app.tar.gz,Steno_0.2.0_aarch64.app.tar.gz.sig,Steno_0.2.0_aarch64.dmg,Steno_0.2.0_x64-setup.exe,Steno_0.2.0_x64-setup.exe.sig,Steno_0.2.0_x64_en-US.msi,Steno_0.2.0_x64_en-US.msi.sig,steno-desktop_0.2.0_amd64.AppImage,steno-desktop_0.2.0_amd64.AppImage.sig,steno-desktop_0.2.0_amd64.deb,steno-desktop_0.2.0_amd64.deb.sig'
  [[ "$(listing "$scratch/all-assets")" == "$wanted" ]] || fail "all three gave $(listing "$scratch/all-assets")"
  [[ "$(cat "$scratch/all-assets/Steno_0.2.0_aarch64.app.tar.gz")" == 'bytes of macos/Steno.app.tar.gz' ]] \
    || fail "the renamed archive holds other bytes"
else
  fail "all three platforms failed: $output"
fi

# One platform.
dir="$(dist linux-only linux)"
"$script" 0.2.0 "$dir" "$scratch/linux-assets" linux >/dev/null 2>&1 || fail "linux alone failed"
[[ "$(listing "$scratch/linux-assets")" == steno-desktop_0.2.0_amd64.AppImage,steno-desktop_0.2.0_amd64.AppImage.sig,steno-desktop_0.2.0_amd64.deb,steno-desktop_0.2.0_amd64.deb.sig ]] \
  || fail "linux alone gave $(listing "$scratch/linux-assets")"

# refuse <why> <dist> <platform...>: exit non-zero with an ::error::, and
# no output line that starts another workflow command.
refuse() {
  local why="$1" dir="$2" output
  shift 2
  if output="$("$script" 0.2.0 "$dir" "$dir-assets" "$@" 2>&1)"; then
    fail "$why was accepted: $output"
  elif [[ "$output" != "::error::"* ]]; then
    fail "$why did not fail with an ::error::: $output"
  elif [[ "$(grep -c '^::' <<< "$output")" != 1 ]]; then
    fail "$why printed more than one workflow command: $output"
  fi
}

dir="$(dist deb-in-windows linux windows)"
cp "$dir/steno-desktop-linux/deb/steno-desktop_0.2.0_amd64.deb" "$dir/steno-desktop-windows/msi/other_amd64.deb"
refuse 'a .deb in the windows artifact' "$dir" linux windows
dir="$(dist appimage-in-macos macos)"
printf 'x\n' > "$dir/steno-desktop-macos/macos/Steno.AppImage"
refuse 'an AppImage in the macos artifact' "$dir" macos
dir="$(dist stray-exe windows)"
printf 'x\n' > "$dir/steno-desktop-windows/nsis/helper.exe"
refuse 'an .exe that is no -setup.exe' "$dir" windows
dir="$(dist extra-artifact linux macos)"
refuse 'an artifact of a platform not named' "$dir" linux
dir="$(dist unknown-artifact linux)"
mkdir -p "$dir/steno-desktop-other/deb"
printf 'x\n' > "$dir/steno-desktop-other/deb/other.deb"
refuse 'an artifact of no platform' "$dir" linux
dir="$(dist missing-artifact linux)"
refuse 'a platform without its artifact' "$dir" linux windows
dir="$(dist duplicate linux)"
cp "$dir/steno-desktop-linux/deb/steno-desktop_0.2.0_amd64.deb" "$dir/steno-desktop-linux/appimage/"
refuse 'two files of one name' "$dir" linux
dir="$(dist unsigned-archive macos)"
rm "$dir/steno-desktop-macos/macos/Steno.app.tar.gz.sig"
refuse 'an archive without its signature' "$dir" macos
dir="$(dist symlink linux)"
ln -s /etc/hostname "$dir/steno-desktop-linux/deb/link.deb"
refuse 'a symlink' "$dir" linux
dir="$(dist newline linux)"
printf 'x\n' > "$dir/steno-desktop-linux/deb/"$'evil\n::warning::x'
refuse 'a name with a newline' "$dir" linux
dir="$(dist not-empty linux)"
mkdir -p "$dir-assets"
printf 'x\n' > "$dir-assets/left-over.deb"
refuse 'an assets directory that is not empty' "$dir" linux
dir="$(dist no-platform linux)"
refuse 'no platform' "$dir"
refuse 'an unknown platform' "$dir" linux freebsd

if ((failures > 0)); then
  echo "release-assets: $failures failed"
  exit 1
fi
echo "release-assets: ok"
