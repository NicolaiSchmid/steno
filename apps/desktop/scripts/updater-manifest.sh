#!/usr/bin/env bash
# The Tauri updater's static manifest (`latest.json`) for one release, from
# the updater artifacts in a directory and the `.sig` beside each: one entry
# per `{os}-{arch}-{installer}` key the updater plugin looks up first, and
# the `{os}-{arch}` key it falls back to when a binary does not know its
# bundle type (the AppImage on Linux, the NSIS installer on Windows).
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
#
# The artifacts, as the desktop release workflow names them:
#   macos    *.app.tar.gz            darwin-aarch64-app, darwin-aarch64
#   linux    *.AppImage              linux-x86_64-appimage, linux-x86_64
#            *.deb                   linux-x86_64-deb
#   windows  *-setup.exe (NSIS)      windows-x86_64-nsis, windows-x86_64
#            *.msi                   windows-x86_64-msi
#
# apps/desktop/scripts/updater-manifest.test.sh checks it; Rust CI runs that.
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

# key<TAB>pattern rows per platform.
rows=""
for platform in "${platforms[@]}"; do
  case "$platform" in
    macos) rows+=$'darwin-aarch64-app\t*.app.tar.gz\ndarwin-aarch64\t*.app.tar.gz\n' ;;
    linux) rows+=$'linux-x86_64-appimage\t*.AppImage\nlinux-x86_64\t*.AppImage\nlinux-x86_64-deb\t*.deb\n' ;;
    windows) rows+=$'windows-x86_64-nsis\t*-setup.exe\nwindows-x86_64\t*-setup.exe\nwindows-x86_64-msi\t*.msi\n' ;;
    *) echo "::error::unknown platform \"$platform\" (known: linux, windows, macos)" >&2; exit 1 ;;
  esac
done

entries=""
while IFS=$'\t' read -r key pattern; do
  [[ -n "$key" ]] || continue
  found="$(artifact "$pattern")"
  entries+="$key"$'\t'"$found"$'\n'
done <<< "$rows"

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
