#!/usr/bin/env bash
# Renders every raster app icon from the one committed vector source,
# Steno/Resources/AppIcon.svg: the ten mac PNGs of the AppIcon appiconset
# (with its Contents.json), the AppIcon.sha256 drift guard beside the SVG,
# and the opaque 1024 px iOS icon in mobile/assets/. Design notes and
# geometry: .plans/2026-09-28-app-icon.md.
#
# Renderer: ImageMagick 7 `magick` with the librsvg delegate, the one tool
# that rasterises the SVG at any pixel size and verifies on macOS and Linux
# alike. ImageMagick's internal MSVG renderer is refused because its gradient
# and anti-aliasing output differs from librsvg's. Never runs in CI: the PNGs
# are committed and built into Assets.car like any other resource, so the
# runners need no Homebrew dependency. `AppIconTests` guards presence, pixel
# size, twin bytes, the mobile icon's opacity and plate, and the SVG hash.
#
# Usage:
#   make-app-icon.sh          rewrite every output in place, one line per file
#   make-app-icon.sh --check  render into a temp dir and cmp against the
#                             committed files; exit 1 and list any difference
set -euo pipefail

check=false
case "${1:-}" in
  "") ;;
  --check) check=true ;;
  *)
    echo "usage: $(basename "$0") [--check]" >&2
    exit 2
    ;;
esac

hint="install ImageMagick with the librsvg delegate: brew install imagemagick (the formula depends on librsvg) or nix-shell -p imagemagick librsvg"
if ! command -v magick >/dev/null 2>&1; then
  echo "error: magick (ImageMagick 7) not found on PATH; $hint" >&2
  exit 1
fi
# grep reads the whole listing (no -q) so magick never dies of SIGPIPE, which
# pipefail would otherwise report as a missing delegate.
if ! magick -list format 2>/dev/null | grep '^ *RSVG' >/dev/null; then
  echo "error: this ImageMagick has no librsvg delegate (only the internal MSVG renderer, whose output differs); $hint" >&2
  exit 1
fi

# Only shell builtins run before the dependency check above, so a machine
# without ImageMagick gets the install hint rather than a stray error; the
# hostless test runs with PATH=/usr/bin:/bin, which on some hosts lacks
# even dirname.
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
app_dir="$(cd "$here/.." && pwd)"
repo_root="$(cd "$app_dir/../.." && pwd)"
svg="$app_dir/Steno/Resources/AppIcon.svg"
digest="$app_dir/Steno/Resources/AppIcon.sha256"
iconset="$app_dir/Steno/Resources/Assets.xcassets/AppIcon.appiconset"
ios_icon="$repo_root/mobile/assets/icon.png"

if [ ! -f "$svg" ]; then
  echo "error: $svg not found" >&2
  exit 1
fi

tmp="$(mktemp -d "${TMPDIR:-/tmp}/steno-app-icon.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

# -strip and the excluded date/time chunks make the bytes deterministic for
# one ImageMagick and librsvg pair, which is what lets --check cmp instead of
# decode. A different pair renders different bytes: re-render and commit.
png_flags=(-strip -define png:exclude-chunks=date,time)

# Density drives librsvg's vector rasterisation at the exact pixel size, so
# no bitmap resampling happens. It must be fractional: bash arithmetic is
# integer and 96 * 16 / 1024 would be 1, which renders 10 x 10 for the 16 px
# slot; every other size divides evenly, which is why the trap hides.
render() { # pixels, output path
  local density
  density="$(awk -v px="$1" 'BEGIN { printf "%.6f", 96 * px / 1024 }')"
  magick -background none -density "$density" "$svg" "${png_flags[@]}" "PNG32:$2"
}

# One table, the five mac point sizes at 1x and 2x, drives the files, the
# Contents.json entries and the list --check compares, so the manifest and
# the set can never disagree. The 32, 256 and 512 px slots are rendered twice
# under two names, which is what Xcode expects: an asset catalog holds no
# symlinks, and a render is deterministic, so the twins are byte-identical.
mkdir -p "$tmp/set"
outputs=(Contents.json)
entries=""
for size in 16 32 128 256 512; do
  for scale in 1 2; do
    suffix=""
    if [ "$scale" = 2 ]; then suffix="@2x"; fi
    name="icon_${size}x${size}${suffix}.png"
    render "$((size * scale))" "$tmp/set/$name"
    outputs+=("$name")
    entries="$entries
    { \"filename\" : \"$name\", \"idiom\" : \"mac\", \"scale\" : \"${scale}x\", \"size\" : \"${size}x${size}\" },"
  done
done

# Rewritten in full rather than patched: that is what makes the step
# idempotent. Xcode's asset editor reformats this file if the set is ever
# opened there; --check then reports it until the script is rerun.
cat > "$tmp/set/Contents.json" <<JSON
{
  "images" : [${entries%,}
  ],
  "info" : {
    "author" : "xcode",
    "version" : 1
  }
}
JSON

# The drift guard AppIconTests compares against the SVG's digest: an edited
# SVG without regenerated PNGs fails on CI, where --check itself never runs.
# It lives beside the SVG, not in the set, where actool would warn about an
# unassigned child.
if command -v shasum >/dev/null 2>&1; then
  shasum -a 256 "$svg" | cut -d' ' -f1 > "$tmp/AppIcon.sha256"
else
  sha256sum "$svg" | cut -d' ' -f1 > "$tmp/AppIcon.sha256"
fi

# iOS: the plate fills the whole 1024 square (viewBox set to the plate, both
# plate radii zeroed) and iOS applies its own mask. The sed matches literal
# attribute values, so a plate edit that renames them would silently keep
# the mac geometry: refuse an unchanged rewrite. App Store Connect rejects a
# 1024 icon with an alpha channel, so it is flattened to RGB here rather
# than trusting Expo's flattening; the opacity probe runs on the render with
# its alpha still attached, since a flattened image is always opaque.
sed -e 's/viewBox="0 0 1024 1024"/viewBox="100 100 824 824"/' \
  -e 's/rx="185"/rx="0"/' -e 's/rx="183"/rx="0"/' "$svg" > "$tmp/ios.svg"
if cmp -s "$svg" "$tmp/ios.svg"; then
  echo "error: the iOS rewrite matched nothing in AppIcon.svg; update the sed in $(basename "$0")" >&2
  exit 1
fi
if [ "$(magick -background none -density 96 "$tmp/ios.svg" -format '%[opaque]' info:)" != "True" ]; then
  echo "error: the iOS plate does not cover the 1024 square; the render has transparent pixels" >&2
  exit 1
fi
magick -background '#141416' -density 96 "$tmp/ios.svg" \
  -alpha remove -alpha off "${png_flags[@]}" "PNG24:$tmp/ios.png"

# Everything below is the only part that touches the tree: --check compares
# each render with its committed file, the default overwrites it.
failed=0
deliver() { # rendered file, committed file
  if [ "$check" = true ]; then
    if ! cmp -s "$1" "$2"; then
      echo "differs or missing: $2" >&2
      failed=1
    fi
  else
    mkdir -p "$(dirname "$2")"
    cp "$1" "$2"
    echo "wrote $2"
  fi
}

for name in "${outputs[@]}"; do
  deliver "$tmp/set/$name" "$iconset/$name"
done
deliver "$tmp/AppIcon.sha256" "$digest"
deliver "$tmp/ios.png" "$ios_icon"

# The set is owned by this script in full: a child Contents.json does not
# name (a renamed slot's old PNG) makes actool warn about an unassigned
# child on every build, so it is reported or removed like a wrong byte.
for path in "$iconset"/*; do
  name="$(basename "$path")"
  case " ${outputs[*]} " in
    *" $name "*) ;;
    *)
      if [ "$check" = true ]; then
        echo "stale: $path" >&2
        failed=1
      else
        rm -f "$path"
        echo "removed $path"
      fi
      ;;
  esac
done

if [ "$failed" -ne 0 ]; then
  echo "the committed icon files do not match AppIcon.svg; run $(basename "$0") without --check" >&2
  exit 1
fi
if [ "$check" = true ]; then
  echo "app icon files match AppIcon.svg"
fi
