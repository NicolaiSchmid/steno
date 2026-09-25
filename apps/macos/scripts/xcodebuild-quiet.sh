#!/usr/bin/env bash
# Runs xcodebuild with its full output in a log file and only the lines a
# human acts on in the console: compiler diagnostics, warnings from the app's
# own sources, test case and suite lines, and the `** ... **` result line.
# Exits non-zero unless the log carries the expected success marker, so a
# crash before the result line never passes. Shared by swift-ci.yml and
# build-release.sh so the filter lives in one place.
#
# Usage: xcodebuild-quiet.sh <log-file> <success-marker> -- <xcodebuild args...>
#   e.g. xcodebuild-quiet.sh build/build.log 'BUILD SUCCEEDED' -- build -scheme Steno ...
set -uo pipefail

log="${1:?log file}"
marker="${2:?success marker, e.g. 'BUILD SUCCEEDED'}"
if [ "${3:-}" != "--" ]; then
  echo "usage: $0 <log> <marker> -- <xcodebuild args>" >&2
  exit 2
fi
shift 3

mkdir -p "$(dirname "$log")"
xcodebuild "$@" 2>&1 \
  | tee "$log" \
  | grep -E --line-buffered '(error:|warning: .*apps/macos|Test Case|Test Suite|\*\* .* \*\*)' \
  || true
grep -q "\*\* $marker \*\*" "$log" || {
  echo "::error::xcodebuild did not report '** $marker **'; see $log" >&2
  exit 1
}
