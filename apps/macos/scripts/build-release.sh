#!/usr/bin/env bash
# Archives and exports Steno.app with Developer ID signing, then verifies
# the signature: `codesign --verify --deep --strict`, and a grep over
# `codesign -dv` for the Developer ID authority, the hardened runtime flag,
# a secure timestamp and exactly the two entitlements (audio input,
# calendars) on the app, then the same team and timestamp on every nested
# code item (Sparkle.framework with its Autoupdate, Updater.app and XPC
# services, the Swift compatibility dylibs Xcode embeds), plus the hardened
# runtime flag on every nested bundle and executable. Removing the grep is
# the reviewer trap named in the plan.
#
# Usage: build-release.sh <marketing-version> <build-number>
#   e.g. build-release.sh 1.2.3 417
#        build-release.sh --verify-only <path/to/Steno.app>
#   runs only the signature checks against an app exported earlier (the
#   hostless tests in ReleaseScriptsTests, a probe on the runner).
# Requires: xcodegen on PATH, the Developer ID Application certificate in an
# unlocked keychain on the search list, Xcode 16.4+.
# Output: apps/macos/dist/Steno.app and apps/macos/dist/Steno.xcarchive
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
app_dir="$(cd "$here/.." && pwd)"
dist="$app_dir/dist"
derived="$app_dir/build/DerivedData"
archive="$dist/Steno.xcarchive"
export_dir="$dist/export"

# Team of one code item after checking its `codesign -dv` details: a secure
# timestamp always, and the hardened runtime flag unless `kind` is
# `library`. Dylibs do not carry the runtime flag (notarisation requires
# it on executables and bundles only), so Xcode's embedded
# libswiftCompatibilitySpan.dylib is signed with `flags=0x0(none)` and must
# not fail the check. Diagnostics go to stderr so a caller capturing the
# team in `$(...)` never swallows them; returns non-zero on failure.
require_release_signature() {
  local item="$1" kind="${2:-executable}" details
  details="$(codesign -dv --verbose=4 "$item" 2>&1)" \
    || { echo "::error::codesign -dv failed on $item" >&2; echo "$details" >&2; return 1; }
  if [ "$kind" != "library" ]; then
    echo "$details" | grep -E '^(CodeDirectory|flags=).*runtime' >/dev/null \
      || { echo "::error::hardened runtime flag missing on $item" >&2; echo "$details" >&2; return 1; }
  fi
  echo "$details" | grep -E '^Timestamp=' >/dev/null \
    || { echo "::error::signature of $item carries no secure timestamp" >&2; echo "$details" >&2; return 1; }
  echo "$details" | sed -n 's/^TeamIdentifier=//p'
}

# `library` for a dylib, `executable` for everything else (bundles and
# standalone Mach-O executables such as Sparkle's Autoupdate).
signature_kind() {
  case "$1" in
    *.dylib) echo library ;;
    *) echo executable ;;
  esac
}

# The signature checks on an exported app. Exits non-zero with an
# `::error::` line on stderr at the first failing check.
verify_release_app() {
  local app="$1" details team entitlements count nested nested_team nested_kind nested_count

  echo "==> verify signature"
  codesign --verify --deep --strict --verbose=2 "$app"

  details="$(codesign -dv --verbose=4 "$app" 2>&1)" \
    || { echo "::error::codesign -dv failed on $app" >&2; echo "$details" >&2; exit 1; }
  echo "$details" | grep -E '^Authority=Developer ID Application' >/dev/null \
    || { echo "::error::not signed with a Developer ID Application certificate" >&2; echo "$details" >&2; exit 1; }
  team="$(require_release_signature "$app")" || exit 1
  test -n "$team" || { echo "::error::no TeamIdentifier on $app" >&2; echo "$details" >&2; exit 1; }

  # `--xml` is the stable machine-readable form; the older `:-` path
  # spelling is deprecated on Xcode 27 and the bare `-` prints a human
  # format with no `<key>` lines, which would make the count below read 0.
  entitlements="$(codesign -d --entitlements - --xml "$app" 2>/dev/null)"
  count="$(echo "$entitlements" | grep -o '<key>' | wc -l | tr -d ' ')"
  echo "$entitlements" | grep -q 'com.apple.security.device.audio-input' \
    || { echo "::error::audio-input entitlement missing" >&2; echo "$entitlements" >&2; exit 1; }
  echo "$entitlements" | grep -q 'com.apple.security.personal-information.calendars' \
    || { echo "::error::calendars entitlement missing" >&2; echo "$entitlements" >&2; exit 1; }
  if echo "$entitlements" | grep -q 'com.apple.security.app-sandbox'; then
    echo "::error::the app must not be sandboxed" >&2; echo "$entitlements" >&2; exit 1
  fi
  if [ "$count" != "2" ]; then
    echo "::error::expected exactly two entitlements, found $count" >&2; echo "$entitlements" >&2; exit 1
  fi

  # Every nested code item carries its own signature: frameworks, the XPC
  # services, helper apps and standalone executables inside them (Sparkle's
  # Autoupdate lives at Sparkle.framework/Versions/B/Autoupdate), and
  # dylibs. The walk descends into frameworks because the helpers sit
  # inside them. `Versions/Current` is a symlink, which find does not
  # follow, so each item is checked once. The whole `-o` disjunction sits
  # in one pair of parentheses ahead of `-print0`; without them find binds
  # the action to the last branch only and never prints the bundles.
  # Notarisation would reject a helper without the hardened runtime; this
  # catches it before the upload.
  echo "==> verify nested signatures (team $team)"
  nested_count=0
  while IFS= read -r -d '' nested; do
    nested_count=$((nested_count + 1))
    nested_kind="$(signature_kind "$nested")"
    nested_team="$(require_release_signature "$nested" "$nested_kind")" || exit 1
    if [ "$nested_team" != "$team" ]; then
      echo "::error::$nested is signed by team '$nested_team', expected '$team'" >&2; exit 1
    fi
    echo "    ok: ${nested#"$app/"} ($nested_kind)"
  done < <(
    find "$app/Contents/Frameworks" \
      \( \
        \( -type d \( -name '*.framework' -o -name '*.xpc' -o -name '*.app' -o -name '*.appex' -o -name '*.bundle' \) \) \
        -o \( -type f \( -name '*.dylib' -o -name 'Autoupdate' \) \) \
      \) -print0 2>/dev/null
  )
  if [ "$nested_count" -eq 0 ]; then
    echo "::error::no nested frameworks found under $app/Contents/Frameworks (Sparkle should be embedded)" >&2; exit 1
  fi
  echo "==> ok: $app (team $team, $nested_count nested items)"
}

if [ "${1:-}" = "--verify-only" ]; then
  app="${2:?path to an exported Steno.app}"
  test -d "$app" || { echo "::error::$app is not an app bundle" >&2; exit 1; }
  verify_release_app "$app"
  exit 0
fi

version="${1:?marketing version, e.g. 1.2.3}"
build="${2:?build number, e.g. 417}"

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

verify_release_app "$app"
echo "==> built $app ($version, build $build)"
