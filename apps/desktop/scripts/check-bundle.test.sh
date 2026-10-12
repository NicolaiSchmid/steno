#!/usr/bin/env bash
# Checks the `app` type of apps/desktop/scripts/check-bundle.sh over stub
# bundles: the merged Info.plist must carry the bundle id of
# tauri.conf.json and the Swift app's SUPublicEDKey of
# `src-tauri/Info.plist` (stable plan S6), which otherwise only a release
# build would check. rust-ci.yml runs it on Linux. `plutil` is the
# system's on macOS and, elsewhere, a stub over Python's plistlib and json
# for the one form the script uses; `codesign` is a stub that records its
# calls, since the unsigned check signs nothing. The expected values are
# the ones `identifier.rs` pins.
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

# bundle <name> <bundle id> [<SUPublicEDKey>]: a bundle directory holding
# macos/Steno.app, its Info.plist with that id and key (none without a
# third argument), the app's executable and a sidecar that greets.
bundle() {
  local dir="$scratch/$1" app key_entry=""
  app="$dir/macos/Steno.app/Contents"
  mkdir -p "$app/MacOS"
  [[ $# -lt 3 ]] || key_entry="<key>SUPublicEDKey</key><string>$3</string>"
  cat > "$app/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>steno-desktop</string>
  <key>CFBundleIdentifier</key><string>$2</string>
  $key_entry
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

if [[ "$failures" -gt 0 ]]; then
  echo "$failures check(s) failed"
  exit 1
fi
echo "check-bundle.sh app: all checks passed"
