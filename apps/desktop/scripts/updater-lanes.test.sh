#!/usr/bin/env bash
# Checks apps/desktop/scripts/updater-lanes.sh, which decides the updater
# lanes the desktop release workflow moves; rust-ci.yml runs it on Linux.
set -euo pipefail

script="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/updater-lanes.sh"
failures=0

fail() {
  echo "FAIL: $1"
  failures=$((failures + 1))
}

# expect <version> <beta> <stable> <lanes>: the lanes it moves, comma-separated.
expect() {
  local got
  got="$("$script" "$1" "$2" "$3" 2>/dev/null | paste -sd, -)" || got="<failed>"
  [[ "$got" == "$4" ]] || fail "$1 over beta '$2', stable '$3' moved '$got', wanted '$4'"
}

# refuse <version> <beta> <stable>: exit 1 with an ::error:: and no lane.
refuse() {
  local output
  if output="$("$script" "$@" 2>&1)"; then
    fail "'$1' '$2' '$3' was accepted: $output"
  elif [[ "$output" != "::error::"* || "$output" == *desktop-* ]]; then
    fail "'$1' '$2' '$3' did not fail with an ::error:: alone: $output"
  fi
}

# No lane yet.
expect 0.2.0-rc.1 '' '' desktop-beta
expect 0.2.0 '' '' desktop-stable,desktop-beta
# Forward.
expect 0.2.0-rc.2 0.2.0-rc.1 '' desktop-beta
expect 0.2.0 0.2.0-rc.2 '' desktop-stable,desktop-beta
expect 0.2.1 0.2.0 0.2.0 desktop-stable,desktop-beta
expect 0.10.0 0.9.0 0.9.0 desktop-stable,desktop-beta
expect 0.3.0-rc.1 0.2.1 0.2.1 desktop-beta
# A rerun of the same tag moves the same lanes.
expect 0.3.0 0.3.0 0.3.0 desktop-stable,desktop-beta
expect 0.3.0-rc.2 0.3.0-rc.2 0.2.1 desktop-beta
# Never backwards.
expect 0.2.1 0.3.0-rc.1 0.2.0 desktop-stable
expect 0.2.2 0.3.0 0.3.0 ''
expect 0.3.0-rc.2 0.3.0 0.3.0 ''
expect 0.2.1-rc.1 0.3.0-rc.1 0.2.0 ''
expect 0.9.0 0.10.0 0.10.0 ''
expect 1.9.9 2.0.0-rc.1 1.9.10 ''
# Pre-release precedence (the SemVer 2.0 example chain), each above the last.
chain=(1.0.0-alpha 1.0.0-alpha.1 1.0.0-alpha.beta 1.0.0-beta 1.0.0-beta.2 1.0.0-beta.11 1.0.0-rc.1)
for ((i = 1; i < ${#chain[@]}; i++)); do
  expect "${chain[i]}" "${chain[i - 1]}" '' desktop-beta
  expect "${chain[i - 1]}" "${chain[i]}" '' ''
done
expect 1.0.0 1.0.0-rc.1 '' desktop-stable,desktop-beta
expect 1.0.0-rc.10 1.0.0-rc.9 '' desktop-beta
expect 1.0.0-rc.99999999999999999999 1.0.0-rc.99999999999999999998 '' desktop-beta
expect 1.0.0-RC.1 1.0.0-rc.1 '' ''
# Build metadata does not count.
expect 0.2.0+2 0.2.0+5 0.2.0+5 desktop-stable,desktop-beta

refuse x '' ''
refuse 0.2 '' ''
refuse 0.2.0 null ''
refuse 0.2.0 '' 'v0.1.0'
refuse 0.2.0-rc..1 '' ''

if ((failures > 0)); then
  echo "updater-lanes: $failures failed"
  exit 1
fi
echo "updater-lanes: ok"
