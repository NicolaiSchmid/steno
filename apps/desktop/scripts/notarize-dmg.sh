#!/usr/bin/env bash
# Notarises the desktop DMG with notarytool, staples the ticket and runs the
# Gatekeeper assessment, as apps/macos/scripts/make-dmg.sh does for the
# Swift app. The Tauri bundler has already signed, notarised and stapled
# the .app inside and signed the image itself; notarising the image too
# lets Gatekeeper pass it offline on first open, before the app is copied.
#
#   ASC_KEY_ID=… ASC_ISSUER_ID=… ASC_KEY_PATH=<AuthKey_….p8> notarize-dmg.sh <dmg>
#
# Writes notarization.json beside the image; on a status other than
# Accepted prints notarytool's log for the submission and exits 1.
set -euo pipefail

dmg="${1:?path to the .dmg}"
: "${ASC_KEY_ID:?ASC_KEY_ID missing}"
: "${ASC_ISSUER_ID:?ASC_ISSUER_ID missing}"
: "${ASC_KEY_PATH:?ASC_KEY_PATH missing}"
test -f "$dmg" || { echo "::error::$dmg missing" >&2; exit 1; }
result="$(dirname "$dmg")/notarization.json"
auth=(--key "$ASC_KEY_PATH" --key-id "$ASC_KEY_ID" --issuer "$ASC_ISSUER_ID")

codesign --verify --verbose=2 "$dmg"

echo "==> notarytool submit"
xcrun notarytool submit "$dmg" "${auth[@]}" --wait --timeout 45m --output-format json > "$result"
status="$(plutil -extract status raw -o - "$result" 2>/dev/null || true)"
if [[ "$status" != Accepted ]]; then
  submission="$(plutil -extract id raw -o - "$result" 2>/dev/null || true)"
  echo "::error::notarisation status: ${status:-unknown} (submission ${submission:-unknown})" >&2
  if [[ -n "$submission" ]]; then
    xcrun notarytool log "$submission" "${auth[@]}" || true
  fi
  exit 1
fi

echo "==> staple"
xcrun stapler staple "$dmg"
xcrun stapler validate "$dmg"

echo "==> gatekeeper assessment"
spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"
