#!/usr/bin/env bash
# Packages dist/Steno.app into a signed DMG, notarises it with notarytool,
# staples the ticket and runs the Gatekeeper assessment.
#
# Usage: make-dmg.sh <marketing-version> [--skip-notarization]
# Environment for notarisation: ASC_KEY_ID, ASC_ISSUER_ID and either
# ASC_PRIVATE_KEY (the .p8 contents) or ASC_PRIVATE_KEY_PATH.
# Output: apps/macos/dist/Steno-<version>.dmg
set -euo pipefail

version="${1:?marketing version, e.g. 1.2.3}"
skip_notarization=false
if [ "${2:-}" = "--skip-notarization" ]; then skip_notarization=true; fi

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
app_dir="$(cd "$here/.." && pwd)"
dist="$app_dir/dist"
app="$dist/Steno.app"
dmg="$dist/Steno-$version.dmg"
staging="$dist/dmg-staging"
identity="${CODESIGN_IDENTITY:-Developer ID Application}"

test -d "$app" || { echo "::error::$app missing; run build-release.sh first"; exit 1; }

echo "==> staging"
rm -rf "$staging" "$dmg"
mkdir -p "$staging"
cp -R "$app" "$staging/Steno.app"
ln -s /Applications "$staging/Applications"

echo "==> hdiutil"
hdiutil create -volname "Steno $version" -srcfolder "$staging" -ov -format UDZO -fs HFS+ "$dmg" >/dev/null
rm -rf "$staging"

echo "==> sign the image"
codesign --sign "$identity" --timestamp --force "$dmg"
codesign --verify --verbose=2 "$dmg"

if [ "$skip_notarization" = true ]; then
  echo "==> notarisation skipped (dry run)"
  echo "$dmg"
  exit 0
fi

: "${ASC_KEY_ID:?ASC_KEY_ID missing}"
: "${ASC_ISSUER_ID:?ASC_ISSUER_ID missing}"
key_path="${ASC_PRIVATE_KEY_PATH:-}"
cleanup_key=false
if [ -z "$key_path" ]; then
  : "${ASC_PRIVATE_KEY:?ASC_PRIVATE_KEY (or ASC_PRIVATE_KEY_PATH) missing}"
  key_path="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/AuthKey_$ASC_KEY_ID.p8"
  umask 077
  printf '%s\n' "$ASC_PRIVATE_KEY" > "$key_path"
  cleanup_key=true
fi
trap '[ "$cleanup_key" = true ] && rm -f "$key_path"' EXIT

echo "==> notarytool submit"
xcrun notarytool submit "$dmg" \
  --key "$key_path" --key-id "$ASC_KEY_ID" --issuer "$ASC_ISSUER_ID" \
  --wait --timeout 45m \
  --output-format json > "$dist/notarization.json"
status="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("status",""))' "$dist/notarization.json")"
if [ "$status" != "Accepted" ]; then
  submission="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("id",""))' "$dist/notarization.json")"
  echo "::error::notarisation status: $status (submission $submission)"
  if [ -n "$submission" ]; then
    xcrun notarytool log "$submission" --key "$key_path" --key-id "$ASC_KEY_ID" --issuer "$ASC_ISSUER_ID" || true
  fi
  exit 1
fi

echo "==> staple"
xcrun stapler staple "$dmg"
xcrun stapler validate "$dmg"

echo "==> gatekeeper assessment"
spctl -a -t open --context context:primary-signature -v "$dmg"

echo "$dmg"
