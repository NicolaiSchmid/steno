#!/usr/bin/env bash
# The build matrix of .github/workflows/desktop-release.yml: the platforms
# named in $PLATFORMS (comma-separated, any case, spaces ignored; linux,
# windows, macos), each with its runner, its bundle targets and the files
# to upload, and the macOS runner from $MACOS_RUNS_ON (a JSON string or
# array, `"macos-15"` when unset). Prints {"include":[...]} on one line, in
# the order below. A name it does not know, or no name at all, is an
# `::error::` and exit 1, so a typo never builds fewer platforms silently.
#
#   PLATFORMS=linux,macos apps/desktop/scripts/release-matrix.sh
#
# apps/desktop/scripts/release-matrix.test.sh checks it; rust-ci.yml runs that.
set -euo pipefail

platforms="${PLATFORMS:-}"
mac="${MACOS_RUNS_ON:-\"macos-15\"}"

# The .app is inside the .dmg; the .app.tar.gz exists only as an updater
# artifact, which the signing key (WP9) switches on, and is uploaded then
# beside its .sig.
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
  '$names - ($all | map(.name)) | unique | join(", ")')"
if [[ -n "$unknown" ]]; then
  echo "::error::unknown platform in '$platforms': $unknown (known: linux, windows, macos)" >&2
  exit 1
fi
matrix="$(jq -c --argjson names "$names" --argjson mac "$mac" \
  '{include: [ .[] | select(.name as $n | $names | index($n)) | if .name == "macos" then .os = $mac else . end ]}' \
  <<< "$all")"
# Every name is known by now, so no name is no platform.
if [[ "$names" == "[]" ]]; then
  echo "::error::no platform selected from '$platforms'" >&2
  exit 1
fi
echo "$matrix"
