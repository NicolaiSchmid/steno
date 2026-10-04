#!/usr/bin/env bash
# Checks apps/desktop/scripts/updater-manifest.sh, the updater manifest the
# desktop release workflow publishes; rust-ci.yml runs it on Linux. Needs jq.
set -euo pipefail

script="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/updater-manifest.sh"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
failures=0
base='https://github.com/NicolaiSchmid/steno/releases/download/desktop-v0.2.0'

fail() {
  echo "FAIL: $1"
  failures=$((failures + 1))
}

# A release directory as the publish job downloads it; a signature is the
# file's name, so a test can tell which file an entry signs. The Windows
# signatures end in CRLF, as a `.sig` written on Windows may.
release="$scratch/release"
mkdir -p "$release"
for file in Steno_0.2.0_aarch64.app.tar.gz Steno_0.2.0_aarch64.dmg \
  steno-desktop_0.2.0_amd64.AppImage steno-desktop_0.2.0_amd64.deb \
  Steno_0.2.0_x64-setup.exe Steno_0.2.0_x64_en-US.msi; do
  printf 'bytes' > "$release/$file"
done
for file in Steno_0.2.0_aarch64.app.tar.gz steno-desktop_0.2.0_amd64.AppImage \
  steno-desktop_0.2.0_amd64.deb; do
  printf 'sig-of-%s\n' "$file" > "$release/$file.sig"
done
for file in Steno_0.2.0_x64-setup.exe Steno_0.2.0_x64_en-US.msi; do
  printf 'sig-of-%s\r\n' "$file" > "$release/$file.sig"
done

manifest="$(PUB_DATE=2026-10-04T00:00:00Z "$script" 0.2.0 "$base/" "$release")"

field() {
  jq -r "$1" <<< "$manifest"
}

[[ "$(field .version)" == 0.2.0 ]] || fail "version is $(field .version)"
[[ "$(field .pub_date)" == 2026-10-04T00:00:00Z ]] || fail "pub_date is $(field .pub_date)"
keys="$(field '.platforms | keys | join(",")')"
wanted='darwin-aarch64,darwin-aarch64-app,linux-x86_64,linux-x86_64-appimage,linux-x86_64-deb,windows-x86_64,windows-x86_64-msi,windows-x86_64-nsis'
[[ "$keys" == "$wanted" ]] || fail "keys are $keys"

# entry <key> <file>: the URL is the base (one slash) plus the file, the
# signature is that file's .sig without its line ending.
entry() {
  local key="$1" file="$2" url signature
  url="$(field ".platforms[\"$key\"].url")"
  signature="$(field ".platforms[\"$key\"].signature")"
  [[ "$url" == "$base/$file" ]] || fail "$key points at $url"
  [[ "$signature" == "sig-of-$file" ]] || fail "$key carries the signature '$signature'"
}
entry darwin-aarch64-app Steno_0.2.0_aarch64.app.tar.gz
entry darwin-aarch64 Steno_0.2.0_aarch64.app.tar.gz
entry linux-x86_64-appimage steno-desktop_0.2.0_amd64.AppImage
entry linux-x86_64 steno-desktop_0.2.0_amd64.AppImage
entry linux-x86_64-deb steno-desktop_0.2.0_amd64.deb
entry windows-x86_64-nsis Steno_0.2.0_x64-setup.exe
entry windows-x86_64 Steno_0.2.0_x64-setup.exe
entry windows-x86_64-msi Steno_0.2.0_x64_en-US.msi

# A file name with a character a URL path cannot carry is percent-encoded.
encoded="$scratch/encoded"
mkdir -p "$encoded"
printf 'bytes' > "$encoded/Steno_0.2.0+5_aarch64.app.tar.gz"
printf 'sig\n' > "$encoded/Steno_0.2.0+5_aarch64.app.tar.gz.sig"
url="$("$script" 0.2.0 "$base" "$encoded" macos | jq -r '.platforms["darwin-aarch64"].url')"
[[ "$url" == "$base/Steno_0.2.0%2B5_aarch64.app.tar.gz" ]] || fail "an encoded name points at $url"

# Only the platforms named.
only="$("$script" 0.2.0 "$base" "$release" linux | jq -r '.platforms | keys | join(",")')"
[[ "$only" == linux-x86_64,linux-x86_64-appimage,linux-x86_64-deb ]] || fail "linux alone gave $only"

# refuse <why> <args...>: exit non-zero with an ::error:: and no manifest.
refuse() {
  local why="$1" output
  shift
  if output="$("$script" "$@" 2>&1)"; then
    fail "$why was accepted: $output"
  elif [[ "$output" != *"::error::"* || "$output" == *'"platforms"'* ]]; then
    fail "$why did not fail with an ::error:: alone: $output"
  fi
}

# variant <name>: a copy of the release directory to break.
variant() {
  cp -R "$release" "$scratch/$1"
  echo "$scratch/$1"
}

refuse 'an unknown platform' 0.2.0 "$base" "$release" freebsd
dir="$(variant missing-sig)"
rm "$dir/steno-desktop_0.2.0_amd64.deb.sig"
refuse 'a missing signature' 0.2.0 "$base" "$dir"
dir="$(variant empty-sig)"
: > "$dir/Steno_0.2.0_x64_en-US.msi.sig"
refuse 'an empty signature' 0.2.0 "$base" "$dir"
dir="$(variant missing)"
rm "$dir/Steno_0.2.0_aarch64.app.tar.gz"
refuse 'a missing artifact' 0.2.0 "$base" "$dir" macos
dir="$(variant two)"
cp "$dir/steno-desktop_0.2.0_amd64.deb" "$dir/steno-desktop_0.1.0_amd64.deb"
cp "$dir/steno-desktop_0.2.0_amd64.deb.sig" "$dir/steno-desktop_0.1.0_amd64.deb.sig"
refuse 'two candidates for one key' 0.2.0 "$base" "$dir" linux

if ((failures > 0)); then
  echo "updater-manifest: $failures failed"
  exit 1
fi
echo "updater-manifest: ok"
