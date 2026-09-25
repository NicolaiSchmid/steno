#!/usr/bin/env bash
# Archives and exports Steno.app with Developer ID signing, then verifies
# the signature: `codesign --verify --deep --strict`, and a grep over
# `codesign -dv --entitlements -` for the Developer ID authority, the
# hardened runtime flag and exactly the two entitlements (audio input,
# calendars). Removing the grep is the reviewer trap named in the plan.
#
# Usage: build-release.sh <marketing-version> <build-number>
#   e.g. build-release.sh 1.2.3 417
# Requires: xcodegen on PATH, the Developer ID Application certificate in an
# unlocked keychain on the search list, Xcode 16.4+.
# Output: apps/macos/dist/Steno.app and apps/macos/dist/Steno.xcarchive
set -euo pipefail

version="${1:?marketing version, e.g. 1.2.3}"
build="${2:?build number, e.g. 417}"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
app_dir="$(cd "$here/.." && pwd)"
dist="$app_dir/dist"
derived="$app_dir/build/DerivedData"
archive="$dist/Steno.xcarchive"
export_dir="$dist/export"

rm -rf "$archive" "$export_dir"
mkdir -p "$dist"

echo "==> xcodegen"
(cd "$app_dir" && xcodegen generate --spec project.yml)

echo "==> archive $version ($build)"
xcodebuild archive \
  -project "$app_dir/Steno.xcodeproj" \
  -scheme Steno \
  -configuration Release \
  -destination 'generic/platform=macOS' \
  -derivedDataPath "$derived" \
  -archivePath "$archive" \
  -skipPackagePluginValidation \
  -skipMacroValidation \
  MARKETING_VERSION="$version" \
  CURRENT_PROJECT_VERSION="$build" \
  | tee "$dist/archive.log" | grep -E '(error:|warning: .*apps/macos|\*\* .* \*\*)' || true
grep -q '\*\* ARCHIVE SUCCEEDED \*\*' "$dist/archive.log"

echo "==> export (developer-id)"
xcodebuild -exportArchive \
  -archivePath "$archive" \
  -exportOptionsPlist "$app_dir/Config/ExportOptions.plist" \
  -exportPath "$export_dir" \
  | tee "$dist/export.log" | grep -E '(error:|\*\* .* \*\*)' || true
grep -q '\*\* EXPORT SUCCEEDED \*\*' "$dist/export.log"

app="$export_dir/Steno.app"
test -d "$app"
rm -rf "$dist/Steno.app"
mv "$app" "$dist/Steno.app"
app="$dist/Steno.app"

echo "==> verify signature"
codesign --verify --deep --strict --verbose=2 "$app"

details="$(codesign -dv --verbose=4 "$app" 2>&1)"
echo "$details" | grep -E '^Authority=Developer ID Application' >/dev/null \
  || { echo "::error::not signed with a Developer ID Application certificate"; echo "$details"; exit 1; }
echo "$details" | grep -E '^(CodeDirectory|flags=).*runtime' >/dev/null \
  || { echo "::error::hardened runtime flag missing"; echo "$details"; exit 1; }
echo "$details" | grep -E '^Timestamp=' >/dev/null \
  || { echo "::error::signature carries no secure timestamp"; echo "$details"; exit 1; }

entitlements="$(codesign -d --entitlements :- "$app" 2>/dev/null || codesign -d --entitlements - "$app" 2>/dev/null)"
count="$(echo "$entitlements" | grep -c '<key>' || true)"
echo "$entitlements" | grep -q 'com.apple.security.device.audio-input' \
  || { echo "::error::audio-input entitlement missing"; echo "$entitlements"; exit 1; }
echo "$entitlements" | grep -q 'com.apple.security.personal-information.calendars' \
  || { echo "::error::calendars entitlement missing"; echo "$entitlements"; exit 1; }
if echo "$entitlements" | grep -q 'com.apple.security.app-sandbox'; then
  echo "::error::the app must not be sandboxed"; echo "$entitlements"; exit 1
fi
if [ "$count" != "2" ]; then
  echo "::error::expected exactly two entitlements, found $count"; echo "$entitlements"; exit 1
fi

# Every nested binary (Sparkle, its XPC services and Autoupdate, FluidAudio's
# xcframework) must carry the same team's signature.
team="$(echo "$details" | sed -n 's/^TeamIdentifier=//p')"
find "$app/Contents/Frameworks" -type d \( -name '*.framework' -o -name '*.xpc' -o -name '*.app' \) -prune -print0 2>/dev/null \
  | while IFS= read -r -d '' nested; do
      nested_team="$(codesign -dv "$nested" 2>&1 | sed -n 's/^TeamIdentifier=//p')"
      if [ "$nested_team" != "$team" ]; then
        echo "::error::$nested is signed by team '$nested_team', expected '$team'"; exit 1
      fi
    done

echo "==> ok: $app ($version, build $build, team $team)"
