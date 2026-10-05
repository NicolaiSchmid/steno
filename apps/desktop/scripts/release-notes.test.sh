#!/usr/bin/env bash
# Checks apps/desktop/scripts/release-notes.sh, the notes of a desktop
# release, against the committed release signing key; rust-ci.yml runs it
# on Linux. Needs gpg.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
script="$here/release-notes.sh"
public_key="$here/../release-signing-key.asc"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
failures=0

fail() {
  echo "FAIL: $1"
  failures=$((failures + 1))
}

# The fingerprint apps/desktop/README.md publishes (Checksums and OpenPGP
# signatures), grouped as gpg prints it.
fingerprint='048B 5279 50E4 F609 B90E  6349 5F88 10A6 E6D4 DB46'

dir="$scratch/assets"
mkdir -p "$dir"
for file in SHA256SUMS SHA256SUMS.asc steno-desktop_0.2.0_amd64.deb steno-desktop_0.2.0_amd64.deb.asc \
  steno-desktop_0.2.0_amd64.AppImage steno-desktop_0.2.0_amd64.AppImage.asc Steno_0.2.0_x64-setup.exe; do
  printf 'bytes\n' > "$dir/$file"
done

notes="$(GITHUB_REPOSITORY=owner/repo "$script" 0.2.0 desktop-v0.2.0 "$dir" "$public_key")"

# has <why> <text>: the notes carry the text verbatim.
has() {
  [[ "$notes" == *"$2"* ]] || fail "the notes lack $1: $2"
}
has 'the version' 'Steno desktop 0.2.0 for macOS'
# shellcheck disable=SC2016 # the backticks are Markdown
has 'the platforms' 'Linux (`.deb`, AppImage) and Windows (`.msi`, NSIS `-setup.exe`)'
has 'the macOS note' "On a Mac, install the app from the repository's latest release for now; this macOS build is a preview."
has 'the Windows note' 'the installers are not code-signed yet, so SmartScreen warns'
has 'the hash check in PowerShell' "(Get-FileHash .\\<installer>).Hash -eq '<its hash in SHA256SUMS>'"
# shellcheck disable=SC2016 # the backticks are Markdown
has 'what SHA256SUMS lists' 'lists every other file of this release but the OpenPGP signatures (`.asc`)'
# shellcheck disable=SC2016 # the backticks are Markdown
has 'where to run the commands' 'into one directory and run these commands there; `wget` fetches the key where `curl` is missing:'
has 'the fingerprint' "    $fingerprint"
has 'the key link at the tag' 'curl -fsSLO https://raw.githubusercontent.com/owner/repo/desktop-v0.2.0/apps/desktop/release-signing-key.asc'
has 'the key import' 'gpg --import release-signing-key.asc'
has 'the checksum check' 'sha256sum --check --ignore-missing SHA256SUMS'
has 'the SHA256SUMS signature' 'gpg --verify SHA256SUMS.asc SHA256SUMS'
has 'the .deb' 'gpg --verify steno-desktop_0.2.0_amd64.deb.asc steno-desktop_0.2.0_amd64.deb'
has 'the AppImage' 'gpg --verify steno-desktop_0.2.0_amd64.AppImage.asc steno-desktop_0.2.0_amd64.AppImage'
# shellcheck disable=SC2016 # the backticks are Markdown
has 'what a good check prints' 'must report "Good signature" from the fingerprint above, and `sha256sum` must print OK for every file you downloaded'
[[ "$(grep -c '^gpg --verify' <<< "$notes")" == 3 ]] || fail "the notes verify $(grep -c '^gpg --verify' <<< "$notes") files"
[[ "$(grep -c '^```' <<< "$notes")" == 2 ]] || fail "the code block is not closed"

# Without GITHUB_REPOSITORY, the key link names this repository.
default="$(env -u GITHUB_REPOSITORY "$script" 0.2.0 desktop-v0.2.0 "$dir" "$public_key")"
[[ "$default" == *'https://raw.githubusercontent.com/NicolaiSchmid/steno/desktop-v0.2.0/'* ]] \
  || fail "the notes without GITHUB_REPOSITORY link no key in NicolaiSchmid/steno"

# refuse <why> <args...>: exit non-zero with an ::error::.
refuse() {
  local why="$1" output
  shift
  if output="$("$script" "$@" 2>&1)"; then
    fail "$why was accepted: $output"
  elif [[ "$output" != *"::error::"* ]]; then
    fail "$why did not fail with an ::error::: $output"
  fi
}
printf 'not a key\n' > "$scratch/garbage.asc"
refuse 'a file that is no key' 0.2.0 desktop-v0.2.0 "$dir" "$scratch/garbage.asc"
unsigned="$scratch/unsigned"
mkdir -p "$unsigned"
printf 'bytes\n' > "$unsigned/SHA256SUMS"
refuse 'a directory without SHA256SUMS.asc' 0.2.0 desktop-v0.2.0 "$unsigned" "$public_key"

if ((failures > 0)); then
  echo "release-notes: $failures failed"
  exit 1
fi
echo "release-notes: ok"
