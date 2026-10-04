#!/usr/bin/env bash
# Checks apps/desktop/scripts/wix-version.sh, the MSI version the desktop
# release workflow passes to the Windows bundle; rust-ci.yml runs it on Linux.
set -euo pipefail

script="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/wix-version.sh"
failures=0

fail() {
  echo "FAIL: $1"
  failures=$((failures + 1))
}

expect() {
  local version="$1" wanted="$2" got
  got="$("$script" "$version" 2>&1)" || got="<failed: $got>"
  [[ "$got" == "$wanted" ]] || fail "$version gave '$got', wanted '$wanted'"
}

# refuse <version>: exit 1 with one line, an ::error::.
refuse() {
  local output
  if output="$("$script" "$1" 2>&1)"; then
    fail "$1 was accepted: $output"
  elif [[ "$output" != "::error::"* || "$output" == *$'\n'* ]]; then
    fail "$1 did not fail with one ::error:: line: $output"
  fi
}

expect 0.2.0 0.2.0.65535
expect 0.2.0-rc.1 0.2.0.1
expect 0.11.0-beta.12 0.11.0.12
expect 1.0.0-pre-release.0 1.0.0.0
expect 0.2.0-rc.65534 0.2.0.65534
expect 255.255.65535 255.255.65535.65535

refuse 0.2.0-rc
refuse 0.2.0-rc.a
refuse 0.2.0-rc.1.2
refuse 0.2.0+5
refuse 0.2.0-rc.1+5
refuse 0.2
refuse 01.2.0
refuse 0.2.0-rc.01
refuse 0.2.0-rc.65535
refuse 256.0.0
refuse 0.256.0
refuse 0.0.65536
refuse 99999999999999999999.0.0

if ((failures > 0)); then
  echo "wix-version: $failures failed"
  exit 1
fi
echo "wix-version: ok"
