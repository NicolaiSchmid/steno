#!/usr/bin/env bash
# The Tauri updater's static manifest (`latest.json`) for one release, from
# the updater artifacts in a directory and the `.sig` beside each: one entry
# per `{os}-{arch}-{installer}` key the updater plugin looks up first, and
# the `{os}-{arch}` key it falls back to when a binary does not know its
# bundle type (the AppImage on Linux, the NSIS `-setup.exe` on Windows).
#
#   apps/desktop/scripts/updater-manifest.sh <version> <base-url> <dir> [platform...]
#
# <base-url> is where the release serves its assets
# (https://github.com/<owner>/<repo>/releases/download/<tag>); each URL is
# that plus the file name. The platforms named after <dir> (linux, windows,
# macos; all three when none is named) must each have every artifact and
# signature, or the script fails naming the missing file, so a manifest
# never offers a platform half its installers. Prints the manifest on
# stdout. PUB_DATE overrides the publication time (RFC 3339, for tests).
# apps/desktop/scripts/updater-manifest.test.sh checks it; rust-ci.yml runs that.
set -euo pipefail

version="${1:?version, e.g. 0.11.0}"
base="${2:?base URL of the release assets}"
dir="${3:?directory holding the artifacts and their .sig files}"
shift 3
platforms=("$@")
[[ ${#platforms[@]} -gt 0 ]] || platforms=(linux windows macos)

# One artifact: exactly one file matching the pattern, and its signature.
# Prints "<file name>\t<signature>".
artifact() {
  local pattern="$1" matches=()
  shopt -s nullglob
  # shellcheck disable=SC2206 # the pattern is a glob on purpose
  matches=("$dir"/$pattern)
  shopt -u nullglob
  if [[ ${#matches[@]} -ne 1 ]]; then
    echo "::error::expected one $pattern in $dir, found ${#matches[@]}" >&2
    return 1
  fi
  if [[ ! -s "${matches[0]}.sig" ]]; then
    echo "::error::${matches[0]##*/}.sig is missing or empty" >&2
    return 1
  fi
  printf '%s\t%s\n' "${matches[0]##*/}" "$(tr -d '\r\n' < "${matches[0]}.sig")"
}

entries=""
# entry <key> <pattern>: one "<key>\t<file name>\t<signature>" line.
entry() {
  entries+="$1"$'\t'"$(artifact "$2")"$'\n'
}

for platform in "${platforms[@]}"; do
  case "$platform" in
    macos)
      entry darwin-aarch64-app '*.app.tar.gz'
      entry darwin-aarch64 '*.app.tar.gz'
      ;;
    linux)
      entry linux-x86_64-appimage '*.AppImage'
      entry linux-x86_64 '*.AppImage'
      entry linux-x86_64-deb '*.deb'
      ;;
    windows)
      entry windows-x86_64-nsis '*-setup.exe'
      entry windows-x86_64 '*-setup.exe'
      entry windows-x86_64-msi '*.msi'
      ;;
    *) echo "::error::unknown platform \"$platform\" (known: linux, windows, macos)" >&2; exit 1 ;;
  esac
done

jq -n \
  --arg version "$version" \
  --arg base "${base%/}" \
  --arg date "${PUB_DATE:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}" \
  --arg entries "$entries" \
  '{
    version: $version,
    notes: "Steno \($version)",
    pub_date: $date,
    platforms: (
      $entries | split("\n") | map(select(. != "") | split("\t"))
      | map({key: .[0], value: {url: "\($base)/\(.[1] | @uri)", signature: .[2]}})
      | from_entries
    )
  }'
