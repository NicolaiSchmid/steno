#!/usr/bin/env bash
# The MSI version of a workspace version, for `bundle.windows.wix.version`:
# WiX takes only `major.minor.patch.build`, numbers only, and the Tauri
# bundler rejects a pre-release that is not a bare number (`0.2.0-rc.1`).
#
#   0.2.0          0.2.0.65535
#   0.2.0-rc.3     0.2.0.3      (one label, then a number up to 65534)
#
# The fourth field is informational: Windows Installer ignores it when it
# compares versions, and the bundler's `main.wxs` declares
# `<MajorUpgrade AllowDowngrades="yes">` (`allowDowngrades` is not set, so
# its default applies). Across labels it does not follow SemVer either
# (`beta.12` gives 12, `rc.1` gives 1).
#
# Anything else, or a major or minor above 255 or a patch above 65535 (the
# WiX limits), is an `::error::` and exit 1, so the release run stops
# before it builds. Prints the MSI version.
#
#   apps/desktop/scripts/wix-version.sh <version>
#
# apps/desktop/scripts/wix-version.test.sh checks it; rust-ci.yml runs that.
set -euo pipefail

version="${1:?version, e.g. 0.2.0-rc.1}"
number='(0|[1-9][0-9]*)'

fail() {
  echo "::error::$version cannot be an MSI version: $1 (X.Y.Z or X.Y.Z-<label>.<N>)" >&2
  exit 1
}

[[ "$version" =~ ^$number\.$number\.$number(-[0-9A-Za-z-]+\.$number)?$ ]] \
  || fail "not a release or a numbered pre-release"
major="${BASH_REMATCH[1]}" minor="${BASH_REMATCH[2]}" patch="${BASH_REMATCH[3]}"
pre="${BASH_REMATCH[5]:-}"

# above <digits> <limit>: compared by length first, so no number overflows.
above() {
  ((${#1} > ${#2})) || { ((${#1} == ${#2})) && [[ "$1" > "$2" ]]; }
}
if above "$major" 255; then fail "the major version is above 255"; fi
if above "$minor" 255; then fail "the minor version is above 255"; fi
if above "$patch" 65535; then fail "the patch version is above 65535"; fi
if [[ -n "$pre" ]] && above "$pre" 65534; then fail "the pre-release number is above 65534"; fi
echo "$major.$minor.$patch.${pre:-65535}"
