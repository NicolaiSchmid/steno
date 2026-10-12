#!/usr/bin/env bash
# Checks the `app` type of apps/desktop/scripts/check-bundle.sh over stub
# bundles: the merged Info.plist must carry the bundle id of
# tauri.conf.json and the Swift app's SUPublicEDKey of
# `src-tauri/Info.plist` (stable plan S6), which otherwise only a release
# build would check; and with `--signed --handoff <build>`, what the Swift
# app's last Sparkle update needs (stable plan S7): the pinned identifier
# and key, CFBundleVersion <build>, team KQB68F43PW in the designated
# requirement and a deep, strict verification. rust-ci.yml runs it on
# Linux. `plutil` is the system's on macOS and, elsewhere, a stub over
# Python's plistlib and json for the one form the script uses. `codesign`
# is a stub: for the unsigned check one that records its calls, since that
# check signs nothing; for the signed one, one that answers as a Developer
# ID signed bundle does, with `xcrun` and `spctl` stubs beside it. The
# expected values are the ones `identifier.rs` pins.
set -euo pipefail

script="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/check-bundle.sh"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
failures=0

fail() {
  echo "FAIL: $1"
  failures=$((failures + 1))
}

identifier=com.nicolaischmid.steno.desktop
key='RxaX7phoHvb7M0P4yaOC7zngDo+lqlOE6Iq89UtOuQI='

mkdir -p "$scratch/bin"
cat > "$scratch/bin/codesign" <<'STUB'
#!/usr/bin/env bash
echo "codesign $*" >> "$CODESIGN_CALLS"
exit 1
STUB
if [[ "$(uname -s)" != Darwin ]]; then
  cat > "$scratch/bin/plutil" <<'STUB'
#!/usr/bin/env python3
# plutil -extract <key> raw -o - <file>: a top-level string of a plist or
# a JSON file, as macOS's plutil prints it; exit 1 when it is missing.
import json, plistlib, sys

args = sys.argv[1:]
if len(args) != 6 or args[0] != "-extract" or args[2:5] != ["raw", "-o", "-"]:
    sys.exit(f"the plutil stub does not take {args}")
key, path = args[1], args[5]
with open(path, "rb") as file:
    data = file.read()
value = (json.loads(data) if data.lstrip().startswith(b"{") else plistlib.loads(data)).get(key)
if not isinstance(value, str):
    sys.exit(f"No value at that key path or invalid key path: {key}")
print(value)
STUB
fi
chmod +x "$scratch/bin/"*

# The signed check's stubs: codesign as it answers for a Developer ID
# signed, notarised bundle. CODESIGN_VERIFY=fail fails the deep, strict
# verification; CODESIGN_TEAM is the team the designated requirement names.
mkdir -p "$scratch/signed-bin"
[[ ! -e "$scratch/bin/plutil" ]] || cp "$scratch/bin/plutil" "$scratch/signed-bin/"
cat > "$scratch/signed-bin/codesign" <<'STUB'
#!/usr/bin/env bash
case "$*" in
  "--verify --deep --strict --verbose=2 "*)
    [[ "${CODESIGN_VERIFY:-ok}" == ok ]] || { echo "${*: -1}: a sealed resource is missing or invalid" >&2; exit 1; } ;;
  "-dv --verbose=4 "*)
    printf '%s\n' "Executable=$3" 'CodeDirectory v=20500 size=1 flags=0x10000(runtime) hashes=1+7 location=embedded' \
      'Authority=Developer ID Application: Nicolai Schmid (KQB68F43PW)' 'Timestamp=10 Oct 2026 at 09:00:00' \
      'TeamIdentifier=KQB68F43PW' >&2 ;;
  "-d --entitlements - --xml "*)
    printf '%s' '<plist><dict><key>com.apple.security.device.audio-input</key><true/><key>com.apple.security.personal-information.calendars</key><true/></dict></plist>' ;;
  "-d -r- "*)
    echo "Executable=$3/Contents/MacOS/steno-desktop" >&2
    echo "designated => identifier \"com.nicolaischmid.steno.desktop\" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */ and certificate leaf[field.1.2.840.113635.100.6.1.13] /* exists */ and certificate leaf[subject.OU] = ${CODESIGN_TEAM:-KQB68F43PW}" ;;
  *) echo "the codesign stub does not take $*" >&2; exit 2 ;;
esac
STUB
# shellcheck disable=SC2016 # the stubs' own arguments
printf '#!/usr/bin/env bash\n[[ "$1 $2" == "stapler validate" ]]\n' > "$scratch/signed-bin/xcrun"
# shellcheck disable=SC2016 # the stubs' own arguments
printf '#!/usr/bin/env bash\n[[ "$1 $2" == "--assess --type" ]]\n' > "$scratch/signed-bin/spctl"
chmod +x "$scratch/signed-bin/"*

# bundle <name> <bundle id> [<SUPublicEDKey> [<CFBundleVersion>]]: a
# bundle directory holding macos/Steno.app, its Info.plist with that id,
# key and build (none of the last two where empty or missing), the app's
# executable and a sidecar that greets.
bundle() {
  local dir="$scratch/$1" app key_entry="" build_entry=""
  app="$dir/macos/Steno.app/Contents"
  mkdir -p "$app/MacOS"
  [[ -z "${3:-}" ]] || key_entry="<key>SUPublicEDKey</key><string>$3</string>"
  [[ -z "${4:-}" ]] || build_entry="<key>CFBundleVersion</key><string>$4</string>"
  cat > "$app/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>steno-desktop</string>
  <key>CFBundleIdentifier</key><string>$2</string>
  $key_entry
  $build_entry
</dict>
</plist>
PLIST
  printf '#!/usr/bin/env bash\n' > "$app/MacOS/steno-desktop"
  printf '#!/usr/bin/env bash\necho %s\n' "'{\"type\":\"ready\"}'" > "$app/MacOS/steno-speech-sidecar"
  chmod +x "$app/MacOS/"*
  echo "$dir"
}

# check <bundle dir>: the script's `app` check with the stubs first on
# PATH; prints stdout and stderr together.
check() {
  PATH="$scratch/bin:$PATH" CODESIGN_CALLS="$scratch/codesign-calls" "$script" "$1" app 2>&1
}

# refused <name> <expected text> <bundle args...>: the check fails, naming
# what is wrong.
refused() {
  local name="$1" expected="$2" output
  shift 2
  if output="$(check "$(bundle "$name" "$@")")"; then
    fail "$name passed: $output"
  elif [[ "$output" != *"$expected"* ]]; then
    fail "$name failed without \"$expected\": $output"
  fi
}

if output="$(check "$(bundle good "$identifier" "$key")")"; then
  [[ "$output" == *"holds CFBundleIdentifier $identifier"* ]] || fail "the bundle id is not reported: $output"
  [[ "$output" == *"holds SUPublicEDKey $key"* ]] || fail "the key is not reported: $output"
else
  fail "the good bundle failed: $output"
fi
refused no-key "SUPublicEDKey ''" "$identifier"
refused other-key "SUPublicEDKey 'c29tZSBvdGhlciBrZXk='" "$identifier" 'c29tZSBvdGhlciBrZXk='
refused earlier-id "CFBundleIdentifier 'uno.schmid.steno.desktop'" uno.schmid.steno.desktop "$key"
[[ ! -e "$scratch/codesign-calls" ]] || fail "the unsigned check ran codesign: $(cat "$scratch/codesign-calls")"

# signed <bundle dir> <check-bundle.sh options...>: the signed check with
# the signed stubs first on PATH; prints stdout and stderr together.
signed() {
  local dir="$1"
  shift
  PATH="$scratch/signed-bin:$PATH" "$script" --signed "$@" "$dir" app 2>&1
}

# handoff_refused <name> <expected text> <bundle args...>: the handoff
# check of build 3400 fails, naming what is wrong. CODESIGN_VERIFY and
# CODESIGN_TEAM pass through to the stub.
handoff_refused() {
  local name="$1" expected="$2" output
  shift 2
  if output="$(signed "$(bundle "$name" "$@")" --handoff 3400)"; then
    fail "$name passed the handoff check: $output"
  elif [[ "$output" != *"::error::"*"$expected"* ]]; then
    fail "$name failed the handoff check without \"$expected\": $output"
  fi
}

handoff_bundle="$(bundle handoff "$identifier" "$key" 3400)"
if output="$(signed "$handoff_bundle" --handoff 3400)"; then
  [[ "$output" == *"is signed by team KQB68F43PW, notarised and stapled"* ]] || fail "the signed check did not run: $output"
  [[ "$output" == *"holds CFBundleVersion 3400"* ]] || fail "the build is not reported: $output"
  [[ "$output" == *"is the handoff bundle, build 3400, team KQB68F43PW"* ]] || fail "the handoff is not reported: $output"
else
  fail "the handoff bundle failed: $output"
fi
# Without --handoff the signed check asks nothing of the build number.
output="$(signed "$(bundle signed-only "$identifier" "$key")")" \
  || fail "the signed check without --handoff failed: $output"
[[ "$output" != *"handoff"* ]] || fail "the signed check without --handoff checked the handoff: $output"

handoff_refused handoff-earlier-build "CFBundleVersion '3399', expected '3400'" "$identifier" "$key" 3399
handoff_refused handoff-no-build "CFBundleVersion '', expected '3400'" "$identifier" "$key"
handoff_refused handoff-earlier-id "CFBundleIdentifier 'uno.schmid.steno.desktop'" uno.schmid.steno.desktop "$key" 3400
handoff_refused handoff-other-key "SUPublicEDKey 'c29tZSBvdGhlciBrZXk='" "$identifier" 'c29tZSBvdGhlciBrZXk=' 3400
handoff_refused handoff-no-key "SUPublicEDKey ''" "$identifier" '' 3400
CODESIGN_TEAM=ABCDE12345 handoff_refused handoff-other-team "does not name team KQB68F43PW" "$identifier" "$key" 3400
CODESIGN_TEAM=KQB68F43PWX handoff_refused handoff-longer-team "does not name team KQB68F43PW" "$identifier" "$key" 3400
CODESIGN_VERIFY=fail handoff_refused handoff-broken-seal "codesign --verify --deep --strict fails" "$identifier" "$key" 3400

# The handoff pins its own values: under a configuration that moved to
# another identifier or key, the `app` check passes and the handoff fails.
moved="$scratch/moved/apps/desktop"
mkdir -p "$moved/scripts" "$moved/src-tauri/linux"
cp "$script" "$moved/scripts/"
other_key='c29tZSBvdGhlciBrZXk='
# moved_config <identifier> <key>: the moved copy's tauri.conf.json and Info.plist.
moved_config() {
  printf '{"identifier": "%s"}\n' "$1" > "$moved/src-tauri/tauri.conf.json"
  printf '<?xml version="1.0" encoding="UTF-8"?>\n<plist version="1.0"><dict><key>SUPublicEDKey</key><string>%s</string></dict></plist>\n' \
    "$2" > "$moved/src-tauri/Info.plist"
}
# moved_check <bundle dir> <expected text>: the moved script passes the
# signed check and fails the handoff, naming <expected text>.
moved_check() {
  local output
  PATH="$scratch/signed-bin:$PATH" "$moved/scripts/check-bundle.sh" --signed "$1" app >/dev/null 2>&1 \
    || fail "the moved configuration's own bundle fails the signed check"
  if output="$(PATH="$scratch/signed-bin:$PATH" "$moved/scripts/check-bundle.sh" --signed --handoff 3400 "$1" app 2>&1)"; then
    fail "a bundle of the moved configuration passed the handoff check: $output"
  elif [[ "$output" != *"$2"* ]]; then
    fail "a bundle of the moved configuration failed the handoff without \"$2\": $output"
  fi
}
moved_config com.example.other "$key"
moved_check "$(bundle moved-id com.example.other "$key" 3400)" "CFBundleIdentifier 'com.example.other', expected '$identifier'"
moved_config "$identifier" "$other_key"
moved_check "$(bundle moved-key "$identifier" "$other_key" 3400)" "SUPublicEDKey '$other_key', expected '$key'"

# The options themselves: --handoff needs --signed and a build number.
if output="$(PATH="$scratch/signed-bin:$PATH" "$script" --handoff 3400 "$handoff_bundle" app 2>&1)"; then
  fail "--handoff without --signed passed: $output"
elif [[ "$output" != *"::error::--handoff checks a signed bundle; pass --signed first"* ]]; then
  fail "--handoff without --signed failed without saying so: $output"
fi
for build in '' 0 3400a -1; do
  if output="$(signed "$handoff_bundle" --handoff "$build")"; then
    fail "--handoff '$build' passed: $output"
  elif [[ "$output" != *"::error::--handoff takes the build number, got '$build'"* ]]; then
    fail "--handoff '$build' failed without saying so: $output"
  fi
done

if [[ "$failures" -gt 0 ]]; then
  echo "$failures check(s) failed"
  exit 1
fi
echo "check-bundle.sh app and --handoff: all checks passed"
