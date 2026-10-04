#!/usr/bin/env bash
# The build matrix of .github/workflows/desktop-release.yml: the platforms
# named in $PLATFORMS (comma-separated, any case, spaces ignored; linux,
# windows, macos), each with its runner, its bundle targets and the files
# to upload, and the macOS runner from $MACOS_RUNS_ON (a JSON string or
# array, `"macos-15"` when unset). Prints {"include":[...]} on one line, in
# the order below. A name it does not know, no name at all, or a runner
# that is not a JSON string or array is an `::error::` and exit 1, so a typo
# never builds fewer platforms silently. The error names only the cleaned-up
# names (folded, spaces and newlines gone), never the raw input, so a
# newline in it cannot start a second workflow command.
#
#   PLATFORMS=linux,macos apps/desktop/scripts/release-matrix.sh
#
# apps/desktop/scripts/release-matrix.test.sh checks it; rust-ci.yml runs that.
set -euo pipefail

platforms="${PLATFORMS:-}"
mac="${MACOS_RUNS_ON:-\"macos-15\"}"

# The .app is inside the .dmg; the .app.tar.gz is the macOS updater
# artifact. Every updater artifact's .sig is uploaded beside it (the
# workflow adds them).
all='[
  {"name":"linux","os":"ubuntu-latest","bundles":"deb,appimage",
   "artifacts":"target/release/bundle/deb/*.deb\ntarget/release/bundle/appimage/*.AppImage"},
  {"name":"windows","os":"windows-latest","bundles":"msi,nsis",
   "artifacts":"target/release/bundle/msi/*.msi\ntarget/release/bundle/nsis/*.exe"},
  {"name":"macos","os":null,"bundles":"app,dmg",
   "artifacts":"target/release/bundle/dmg/*.dmg\ntarget/release/bundle/macos/*.app.tar.gz"}
]'

names="$(jq -cn --arg wanted "$platforms" \
  '$wanted | ascii_downcase | split(",") | map(gsub("\\s"; "")) | map(select(. != ""))')"
unknown="$(jq -rn --argjson names "$names" --argjson all "$all" \
  '$names - ($all | map(.name)) | unique | map(tojson) | join(", ")')"
if [[ -n "$unknown" ]]; then
  echo "::error::unknown platform $unknown (known: linux, windows, macos)" >&2
  exit 1
fi
if [[ "$names" == "[]" ]]; then
  echo "::error::no platform selected (known: linux, windows, macos)" >&2
  exit 1
fi
if ! jq -en --argjson mac "$mac" '$mac | type == "string" or type == "array"' >/dev/null 2>&1; then
  echo '::error::MACOS_RUNS_ON must be a JSON string or array of runner labels, such as "macos-15" or ["self-hosted","macOS"]' >&2
  exit 1
fi
jq -c --argjson names "$names" --argjson mac "$mac" \
  '{include: [ .[] | select(.name as $n | $names | index($n)) | if .name == "macos" then .os = $mac else . end ]}' \
  <<< "$all"
