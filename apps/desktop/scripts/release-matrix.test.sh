#!/usr/bin/env bash
# Checks apps/desktop/scripts/release-matrix.sh, the platform filter of the
# desktop release workflow; rust-ci.yml runs it on Linux. Needs jq.
set -euo pipefail

script="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/release-matrix.sh"
failures=0

names() {
  PLATFORMS="$1" MACOS_RUNS_ON='"macos-15"' "$script" | jq -r '[.include[].name] | join(",")'
}

expect() {
  local input="$1" wanted="$2" got
  got="$(names "$input")" || got="<failed>"
  if [[ "$got" != "$wanted" ]]; then
    echo "FAIL: '$input' gave '$got', wanted '$wanted'"
    failures=$((failures + 1))
  fi
}

refuse() {
  local input="$1" output
  if output="$(PLATFORMS="$input" "$script" 2>&1)"; then
    echo "FAIL: '$input' was accepted: $output"
    failures=$((failures + 1))
  elif [[ "$output" != *"::error::"* ]]; then
    echo "FAIL: '$input' failed without an ::error:: line: $output"
    failures=$((failures + 1))
  fi
}

expect 'linux,windows,macos' 'linux,windows,macos'
expect 'macos,linux' 'linux,macos'
expect ' Linux , MACOS ' 'linux,macos'
expect 'windows' 'windows'
expect 'linux,,linux' 'linux'
refuse ''
refuse ','
refuse 'linux,freebsd'
refuse 'mac'
refuse 'linux windows'

runner="$(PLATFORMS=macos MACOS_RUNS_ON='["self-hosted","forge"]' "$script" | jq -c '.include[0].os')"
if [[ "$runner" != '["self-hosted","forge"]' ]]; then
  echo "FAIL: the macOS runner is $runner"
  failures=$((failures + 1))
fi
uploads="$(PLATFORMS=macos "$script" | jq -r '.include[0].artifacts')"
if [[ "$uploads" != *'*.dmg'* || "$uploads" != *'*.app.tar.gz'* ]]; then
  echo "FAIL: the macOS uploads are $uploads"
  failures=$((failures + 1))
fi

if ((failures > 0)); then
  echo "release-matrix: $failures failed"
  exit 1
fi
echo "release-matrix: ok"
