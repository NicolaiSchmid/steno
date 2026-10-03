#!/usr/bin/env bash
# Checks apps/desktop/scripts/release-matrix.sh, the platform filter of the
# desktop release workflow; rust-ci.yml runs it on Linux. Needs jq.
set -euo pipefail

script="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/release-matrix.sh"
failures=0

fail() {
  echo "FAIL: $1"
  failures=$((failures + 1))
}

names() {
  PLATFORMS="$1" MACOS_RUNS_ON='"macos-15"' "$script" | jq -r '[.include[].name] | join(",")'
}

expect() {
  local input="$1" wanted="$2" got
  got="$(names "$input")" || got="<failed>"
  if [[ "$got" != "$wanted" ]]; then
    fail "'$input' gave '$got', wanted '$wanted'"
  fi
}

# refuse <platforms> [macos-runs-on]: exit 1 with one line, an ::error::.
refuse() {
  local input="$1" mac="${2:-\"macos-15\"}" output
  if output="$(PLATFORMS="$input" MACOS_RUNS_ON="$mac" "$script" 2>&1)"; then
    fail "'$input' ($mac) was accepted: $output"
  elif [[ "$output" != "::error::"* || "$output" == *$'\n'* ]]; then
    fail "'$input' ($mac) did not fail with one ::error:: line: $output"
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
# A newline cannot inject a workflow command: the error is one line, and
# the injected text shows only inside it, joined to its neighbour once the
# newline is gone.
refuse $'linux\n::warning::injected'
refuse $'freebsd\r\n::add-mask::x,linux'
injected="$(PLATFORMS=$'linux\n::warning::injected' "$script" 2>&1 || true)"
if [[ "$injected" != '::error::unknown platform "linux::warning::injected" (known: linux, windows, macos)' ]]; then
  fail "the injected name reads $injected"
fi
# A runner that is not a JSON string or array.
refuse 'macos' 'macos-15'
refuse 'linux' '{"label":"macos-15"}'

runner="$(PLATFORMS=macos MACOS_RUNS_ON='["self-hosted","forge"]' "$script" | jq -c '.include[0].os')"
if [[ "$runner" != '["self-hosted","forge"]' ]]; then
  fail "the macOS runner is $runner"
fi
uploads="$(PLATFORMS=macos "$script" | jq -r '.include[0].artifacts')"
if [[ "$uploads" != *'*.dmg'* || "$uploads" != *'*.app.tar.gz'* ]]; then
  fail "the macOS uploads are $uploads"
fi

if ((failures > 0)); then
  echo "release-matrix: $failures failed"
  exit 1
fi
echo "release-matrix: ok"
