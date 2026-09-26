#!/usr/bin/env bash
# Generates the Sparkle appcast for one release from the DMG in dist/.
# The EdDSA private key comes from SPARKLE_PRIVATE_KEY on stdin
# (`generate_appcast --ed-key-file -`), never from the keychain or a file.
# Each release's appcast contains that release only (`--maximum-deltas 0`),
# published as a release asset that `SUFeedURL` resolves through
# `releases/latest/download/appcast.xml`.
#
# Usage: make-appcast.sh <tag>            e.g. make-appcast.sh v1.2.3
# Environment: SPARKLE_PRIVATE_KEY (base64 EdDSA private key from
# `generate_keys -x`). Requires the Sparkle package resolved in DerivedData
# (build-release.sh does that).
# Output: apps/macos/dist/appcast.xml
set -euo pipefail

tag="${1:?release tag, e.g. v1.2.3}"
: "${SPARKLE_PRIVATE_KEY:?SPARKLE_PRIVATE_KEY missing}"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
app_dir="$(cd "$here/.." && pwd)"
dist="$app_dir/dist"
derived="$app_dir/build/DerivedData"
repo="https://github.com/NicolaiSchmid/steno"

generate_appcast="$(find "$derived/SourcePackages/artifacts" -type f -name generate_appcast -path '*/bin/*' 2>/dev/null | head -n 1 || true)"
if [ -z "$generate_appcast" ] || [ ! -x "$generate_appcast" ]; then
  echo "::error::generate_appcast not found under $derived/SourcePackages/artifacts" >&2
  exit 1
fi

# Only the DMG may sit in the folder generate_appcast scans.
work="$dist/appcast-work"
rm -rf "$work"
mkdir -p "$work"
shopt -s nullglob
dmgs=("$dist"/Steno-*.dmg)
shopt -u nullglob
[ "${#dmgs[@]}" -eq 1 ] || { echo "::error::expected exactly one Steno-*.dmg in $dist, found ${#dmgs[@]}"; exit 1; }
cp "${dmgs[0]}" "$work/"

echo "==> generate_appcast"
printf '%s' "$SPARKLE_PRIVATE_KEY" | "$generate_appcast" \
  --ed-key-file - \
  --download-url-prefix "$repo/releases/download/$tag/" \
  --link "$repo/releases" \
  --maximum-deltas 0 \
  -o "$dist/appcast.xml" \
  "$work"
rm -rf "$work"

test -s "$dist/appcast.xml"
grep -q 'sparkle:edSignature' "$dist/appcast.xml" || { echo "::error::appcast carries no EdDSA signature"; exit 1; }
grep -q "$repo/releases/download/$tag/" "$dist/appcast.xml" || { echo "::error::appcast enclosure URL does not point at the release"; exit 1; }
echo "$dist/appcast.xml"
