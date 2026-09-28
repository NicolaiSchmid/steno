#!/usr/bin/env bash
# Renders every raster app icon from the one committed vector source,
# Steno/Resources/AppIcon.svg: the ten mac PNGs of the AppIcon appiconset
# (with its Contents.json), the set's SOURCE.sha256 drift guard, and the
# opaque 1024 px iOS icon in mobile/assets/. Design notes and geometry:
# .plans/2026-09-28-app-icon.md.
#
# Renderer: ImageMagick 7 `magick` with the librsvg delegate, the one tool
# that renders, resizes, strips alpha and verifies on macOS and Linux alike.
# ImageMagick's internal MSVG renderer is refused because its gradient and
# anti-aliasing output differs from librsvg's. Never runs in CI: the PNGs are
# committed and built into Assets.car like any other resource, so the runners
# need no Homebrew dependency. `AppIconTests` guards presence, pixel size, the
# mobile icon's opacity and the SVG hash.
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
# without ImageMagick always gets the install hint rather than a stray error.
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SVG="$here/../Steno/Resources/AppIcon.svg"
SET="$here/../Steno/Resources/Assets.xcassets/AppIcon.appiconset"
IOS="$here/../../../mobile/assets/icon.png"

if [ ! -f "$SVG" ]; then
  echo "error: $SVG not found" >&2
  exit 1
fi

tmp="$(mktemp -d "${TMPDIR:-/tmp}/steno-app-icon.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

# Density drives librsvg's vector rasterisation at the exact pixel size, so
# no bitmap resampling happens. It must be fractional: bash arithmetic is
# integer and 96 * 16 / 1024 would be 1, which renders 10 x 10 for the 16 px
# slot; every other size divides evenly, which is why the trap hides.
render() { # pixels, output path
  local density
  density="$(awk -v px="$1" 'BEGIN { printf "%.6f", 96 * px / 1024 }')"
  magick -background none -density "$density" "$SVG" \
    -strip -define png:exclude-chunks=date,time "PNG32:$2"
}

# One table, the five mac point sizes at 1x and 2x, drives the files, the
# Contents.json entries and the list --check compares, so the manifest and
# the set can never disagree. The 32, 256 and 512 px slots are rendered twice
# under two names, which is what Xcode expects: an asset catalog holds no
# symlinks, and a render is deterministic, so the twins are byte-identical.
mkdir -p "$tmp/set"
outputs=(Contents.json SOURCE.sha256)
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
# idempotent.
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
if command -v shasum >/dev/null 2>&1; then
  shasum -a 256 "$SVG" | cut -d' ' -f1 > "$tmp/set/SOURCE.sha256"
else
  sha256sum "$SVG" | cut -d' ' -f1 > "$tmp/set/SOURCE.sha256"
fi

# iOS: the plate fills the whole 1024 square (viewBox set to the plate, both
# plate radii zeroed) and iOS applies its own mask. App Store Connect rejects
# a 1024 icon with an alpha channel, so it is flattened to RGB here rather
# than trusting Expo's flattening.
sed -e 's/viewBox="0 0 1024 1024"/viewBox="100 100 824 824"/' \
  -e 's/rx="185"/rx="0"/' -e 's/rx="183"/rx="0"/' "$SVG" > "$tmp/ios.svg"
magick -background '#141416' -density 96 "$tmp/ios.svg" \
  -alpha remove -alpha off -strip -define png:exclude-chunks=date,time "PNG24:$tmp/ios.png"
if [ "$(magick identify -format '%[opaque]' "$tmp/ios.png")" != "True" ]; then
  echo "error: the iOS icon render still has transparent pixels" >&2
  exit 1
fi

# Everything below is the only part that touches the tree: --check compares
# each render with its committed file, the default overwrites it.
failed=0
deliver() { # rendered file, committed file
  if [ "$check" = true ]; then
    if ! cmp -s "$1" "$2"; then
      echo "differs: $2"
      failed=1
    fi
  else
    mkdir -p "$(dirname "$2")"
    cp "$1" "$2"
    echo "wrote $2"
  fi
}

for name in "${outputs[@]}"; do
  deliver "$tmp/set/$name" "$SET/$name"
done
deliver "$tmp/ios.png" "$IOS"

if [ "$failed" -ne 0 ]; then
  echo "the committed icon files do not match AppIcon.svg; run $(basename "$0") without --check" >&2
  exit 1
fi
if [ "$check" = true ]; then
  echo "app icon files match AppIcon.svg"
fi
