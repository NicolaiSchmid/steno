#!/usr/bin/env bash
# Archives and exports Steno.app with Developer ID signing, then verifies
# the signature: `codesign --verify --deep --strict`, and a grep over
# `codesign -dv` for the Developer ID authority, the hardened runtime flag,
# a secure timestamp and exactly the two entitlements (audio input,
# calendars) on the app, then the same team, runtime flag and timestamp on
# every nested code item (Sparkle.framework with its Autoupdate, Updater.app
# and XPC services, FluidAudio's framework). Removing the grep is the
# reviewer trap named in the plan.
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
"$here/xcodebuild-quiet.sh" "$dist/archive.log" 'ARCHIVE SUCCEEDED' -- archive \
  -project "$app_dir/Steno.xcodeproj" \
  -scheme Steno \
  -configuration Release \
  -destination 'generic/platform=macOS' \
  -derivedDataPath "$derived" \
  -archivePath "$archive" \
  -skipPackagePluginValidation \
  -skipMacroValidation \
  MARKETING_VERSION="$version" \
  CURRENT_PROJECT_VERSION="$build"

echo "==> export (developer-id)"
"$here/xcodebuild-quiet.sh" "$dist/export.log" 'EXPORT SUCCEEDED' -- -exportArchive \
  -archivePath "$archive" \
  -exportOptionsPlist "$app_dir/Config/ExportOptions.plist" \
  -exportPath "$export_dir"

app="$export_dir/Steno.app"
test -d "$app"
rm -rf "$dist/Steno.app"
mv "$app" "$dist/Steno.app"
app="$dist/Steno.app"

echo "==> verify signature"
codesign --verify --deep --strict --verbose=2 "$app"

# Team, hardened runtime and secure timestamp of one code item, from
# `codesign -dv`. Prints the failing detail block and exits non-zero.
require_release_signature() {
  local item="$1" details
  details="$(codesign -dv --verbose=4 "$item" 2>&1)"
  echo "$details" | grep -E '^(CodeDirectory|flags=).*runtime' >/dev/null \
    || { echo "::error::hardened runtime flag missing on $item"; echo "$details"; exit 1; }
  echo "$details" | grep -E '^Timestamp=' >/dev/null \
    || { echo "::error::signature of $item carries no secure timestamp"; echo "$details"; exit 1; }
  echo "$details" | sed -n 's/^TeamIdentifier=//p'
}

details="$(codesign -dv --verbose=4 "$app" 2>&1)"
echo "$details" | grep -E '^Authority=Developer ID Application' >/dev/null \
  || { echo "::error::not signed with a Developer ID Application certificate"; echo "$details"; exit 1; }
team="$(require_release_signature "$app")"
test -n "$team" || { echo "::error::no TeamIdentifier on $app"; echo "$details"; exit 1; }

# `--xml` is the stable machine-readable form; the older `:-` path spelling
# is deprecated on Xcode 27 and the bare `-` prints a human format with no
# `<key>` lines, which would make the count below read 0.
entitlements="$(codesign -d --entitlements - --xml "$app" 2>/dev/null)"
count="$(echo "$entitlements" | grep -o '<key>' | wc -l | tr -d ' ')"
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

# Every nested code item carries its own signature: frameworks, the XPC
# services, helper apps and standalone executables inside them (Sparkle's
# Autoupdate lives at Sparkle.framework/Versions/B/Autoupdate), and dylibs.
# The walk descends into frameworks because the helpers sit inside them.
# `Versions/Current` is a symlink, which find does not follow, so each item
# is checked once.
# Notarisation would reject a helper without the hardened runtime; this
# catches it before the upload.
echo "==> verify nested signatures (team $team)"
nested_count=0
while IFS= read -r -d '' nested; do
  nested_count=$((nested_count + 1))
  nested_team="$(require_release_signature "$nested")"
  if [ "$nested_team" != "$team" ]; then
    echo "::error::$nested is signed by team '$nested_team', expected '$team'"; exit 1
  fi
  echo "    ok: ${nested#"$app/"}"
done < <(
  find "$app/Contents/Frameworks" \
    \( -type d \( -name '*.framework' -o -name '*.xpc' -o -name '*.app' -o -name '*.appex' -o -name '*.bundle' \) \) \
    -o \( -type f \( -name '*.dylib' -o -name 'Autoupdate' \) \) \
    -print0 2>/dev/null
)
if [ "$nested_count" -eq 0 ]; then
  echo "::error::no nested frameworks found under $app/Contents/Frameworks (Sparkle should be embedded)"; exit 1
fi

echo "==> ok: $app ($version, build $build, team $team, $nested_count nested items)"
