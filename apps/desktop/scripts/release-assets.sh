#!/usr/bin/env bash
# The release's assets in one flat directory, from the bundle jobs'
# artifacts as actions/download-artifact leaves them without
# merge-multiple: <dist>/steno-desktop-<platform>/<bundle type>/<file>.
# Each platform named is taken from its own artifact, and from it only the
# files that platform builds:
#
#   linux    `.deb`, `.AppImage` and the `.sig` of each
#   windows  `.msi`, NSIS `-setup.exe` and the `.sig` of each
#   macos    `.dmg`, `Steno.app.tar.gz` and its `.sig`
#
# The macOS updater archive is `Steno.app.tar.gz` on every version; it
# becomes `Steno_<version>_aarch64.app.tar.gz` like the other assets (its
# signature covers the bytes, not the name). Anything else is an
# `::error::` and exit 1: an artifact of a platform not named, a platform
# without its artifact, a file its platform does not build, two files of
# one name, a non-empty <assets>. So only the Linux artifact's `.deb` and
# `.AppImage` reach the OpenPGP signing (release-signatures.sh).
#
#   apps/desktop/scripts/release-assets.sh <version> <dist> <assets> <platform...>
#
# Names from an artifact appear in errors quoted (`printf %q`), so a newline
# in one cannot start a second workflow command.
# apps/desktop/scripts/release-assets.test.sh checks it; rust-ci.yml runs that.
set -euo pipefail

version="${1:?version, e.g. 0.11.0}"
dist="${2:?directory the steno-desktop-* artifacts were downloaded into}"
assets="${3:?directory to gather the assets in}"
shift 3
platforms=("$@")

error() {
  echo "::error::$1" >&2
  exit 1
}
shown() {
  printf '%q' "$1"
}

[[ ${#platforms[@]} -gt 0 ]] || error "no platform named (known: linux, windows, macos)"
for platform in "${platforms[@]}"; do
  case "$platform" in
    linux | windows | macos) ;;
    *) error "unknown platform $(shown "$platform") (known: linux, windows, macos)" ;;
  esac
done
[[ -d "$dist" ]] || error "$dist is not a directory"
mkdir -p "$assets"
[[ -z "$(ls -A "$assets")" ]] || error "$assets is not empty"

# Every artifact downloaded is one of a platform named.
while IFS= read -r -d '' entry; do
  name="${entry##*/}"
  wanted=false
  for platform in "${platforms[@]}"; do
    [[ "$name" == "steno-desktop-$platform" ]] && wanted=true
  done
  [[ "$wanted" == true && -d "$entry" && ! -L "$entry" ]] \
    || error "unexpected artifact $(shown "$name") (platforms: ${platforms[*]})"
done < <(find "$dist" -mindepth 1 -maxdepth 1 -print0)

for platform in "${platforms[@]}"; do
  artifact="steno-desktop-$platform"
  [[ -d "$dist/$artifact" ]] || error "no $artifact artifact"
  while IFS= read -r -d '' file; do
    path="${file#"$dist/"}"
    [[ -f "$file" && ! -L "$file" ]] || error "$(shown "$path") is not a regular file"
    base="${file##*/}"
    case "$platform:$base" in
      linux:*.deb | linux:*.AppImage | linux:*.deb.sig | linux:*.AppImage.sig) target="$base" ;;
      windows:*.msi | windows:*-setup.exe | windows:*.msi.sig | windows:*-setup.exe.sig) target="$base" ;;
      macos:*.dmg) target="$base" ;;
      macos:Steno.app.tar.gz) target="Steno_${version}_aarch64.app.tar.gz" ;;
      macos:Steno.app.tar.gz.sig) target="Steno_${version}_aarch64.app.tar.gz.sig" ;;
      *) error "$(shown "$path") is no file the $platform build makes" ;;
    esac
    [[ ! -e "$assets/$target" ]] || error "two assets named $(shown "$target"); the second is $(shown "$path")"
    mv -- "$file" "$assets/$target"
  done < <(find "$dist/$artifact" -mindepth 1 ! -type d -print0 | LC_ALL=C sort -z)
done

archive="Steno_${version}_aarch64.app.tar.gz"
if [[ -f "$assets/$archive" && ! -f "$assets/$archive.sig" ]]; then
  error "Steno.app.tar.gz came without Steno.app.tar.gz.sig"
fi
