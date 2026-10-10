#!/usr/bin/env bash
# Checks apps/desktop/scripts/release-notes.sh, the notes of a release,
# against the committed release signing key: the parts every release
# carries, and the paragraphs only the 0.11.0 candidates and 0.11.0 carry
# (stable plan S7). rust-ci.yml runs it on Linux. Needs gpg.
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

notes="$(GITHUB_REPOSITORY=owner/repo "$script" 0.2.0 v0.2.0 "$dir" "$public_key")"

# has <why> <text>: the notes carry the text verbatim.
has() {
  [[ "$notes" == *"$2"* ]] || fail "the notes lack $1: $2"
}
has 'the version' 'Steno 0.2.0 for macOS (Apple silicon, signed and notarised)'
# shellcheck disable=SC2016 # the backticks are Markdown
has 'the platforms' 'Linux (`.deb`, AppImage) and Windows (`.msi`, NSIS `-setup.exe`)'
# lacks <why> <text> [<notes>]: the notes do not carry the text.
lacks() {
  [[ "${3-$notes}" != *"$2"* ]] || fail "the notes carry $1: $2"
}
lacks 'the preview note' 'preview'
lacks 'the pointer to the latest release' 'latest release'
has 'the Windows note' 'the installers are not code-signed yet, so SmartScreen warns'
has 'the hash check in PowerShell' "(Get-FileHash .\\<installer>).Hash -eq '<its hash in SHA256SUMS>'"
# shellcheck disable=SC2016 # the backticks are Markdown
has 'what SHA256SUMS lists' 'lists every other file of this release but the OpenPGP signatures (`.asc`) and `appcast.xml`. It and'
# shellcheck disable=SC2016 # the backticks are Markdown
has 'where to run the commands' 'into one directory and run these commands there; `wget` fetches the key where `curl` is missing:'
has 'the fingerprint' "    $fingerprint"
has 'the key link at the tag' 'curl -fsSLO https://raw.githubusercontent.com/owner/repo/v0.2.0/apps/desktop/release-signing-key.asc'
has 'the key import' 'gpg --import release-signing-key.asc'
has 'the checksum check' 'sha256sum --check --ignore-missing SHA256SUMS'
has 'the SHA256SUMS signature' 'gpg --verify SHA256SUMS.asc SHA256SUMS'
has 'the .deb' 'gpg --verify steno-desktop_0.2.0_amd64.deb.asc steno-desktop_0.2.0_amd64.deb'
has 'the AppImage' 'gpg --verify steno-desktop_0.2.0_amd64.AppImage.asc steno-desktop_0.2.0_amd64.AppImage'
# shellcheck disable=SC2016 # the backticks are Markdown
has 'what a good check prints' 'must report "Good signature" from the fingerprint above, and `sha256sum` must print OK for every file you downloaded'
[[ "$(grep -c '^gpg --verify' <<< "$notes")" == 3 ]] || fail "the notes verify $(grep -c '^gpg --verify' <<< "$notes") files"
[[ "$(grep -c '^```' <<< "$notes")" == 2 ]] || fail "the code block is not closed"

lacks 'the desktop-id paragraph' 'com.nicolaischmid.steno.desktop'
lacks 'the handoff paragraph' 'From the Swift app'

# notes_for <version>: the notes of that version over the same assets.
notes_for() {
  "$script" "$1" "v$1" "$dir" "$public_key"
}

# A candidate publishes no appcast.xml.
candidate="$(notes_for 0.2.0-rc.1)"
# shellcheck disable=SC2016 # the backticks are Markdown
[[ "$candidate" == *'but the OpenPGP signatures (`.asc`). It and each Linux bundle'* ]] \
  || fail "a candidate's SHA256SUMS line is not the one without appcast.xml"

# Every 0.11.0 candidate and 0.11.0 carry the desktop-id paragraph; only
# 0.11.0 the handoff.
# shellcheck disable=SC2016 # the backticks are Markdown
desktop_id=(
  '**From an earlier Steno desktop build** (0.1.0)'
  'the app'"'"'s identifier is now `com.nicolaischmid.steno.desktop`, and such an install converts to it'
  'asks once more for its permissions (microphone, system audio, calendar) and once for each of its keychain items; choose *Always Allow*'
  'keeps two copies of one app once both have updated; the one outside `/Applications` can be deleted'
  'An earlier desktop build that has not updated yet cannot open Steno'"'"'s data once this version has, and quits at launch: install the current build over it by hand.'
)
# shellcheck disable=SC2016 # the backticks are Markdown
handoff=(
  '### From the Swift app'
  'receives this release as an update'
  'and for the login keychain password once for each secret Steno stored; choose *Always Allow*'
  'An old "Steno" entry in System Settings > General > Login Items may need removing'
  'offers *Try again* in Settings > Phones'
  '*Pair again*'
  'The optional audio mixdown is now WAV'
  'Steno now transcribes with Parakeet v3'
  'brew upgrade --greedy --cask nicolaischmid/tap/steno'
  '`yay -S steno-desktop-bin`'
  'inputs.steno.url = "github:NicolaiSchmid/steno/v0.11.0";'
  'On KDE Plasma a logout does not wait for a recording until Plasma calls the desktop portal'"'"'s session monitor'
  'everything that plays on the default output'
  'WebKitGTK'"'"'s file descriptor leak'
  'GNOME shows no tray icon without the AppIndicator extension'
  'one started outside `uwsm` runs in Hyprland'"'"'s own unit'
)
for version in 0.11.0-rc.1 0.11.0-rc.12 0.11.0 0.11.1 0.12.0-rc.1 0.10.0; do
  text="$(notes_for "$version")"
  for line in "${desktop_id[@]}"; do
    case "$version" in
      0.11.0 | 0.11.0-rc.*) [[ "$text" == *"$line"* ]] || fail "$version lacks the desktop-id line: $line" ;;
      *) lacks "the desktop-id line in $version" "$line" "$text" ;;
    esac
  done
  for line in "${handoff[@]}"; do
    if [[ "$version" == 0.11.0 ]]; then
      [[ "$text" == *"$line"* ]] || fail "0.11.0 lacks the handoff line: $line"
    else
      lacks "the handoff line in $version" "$line" "$text"
    fi
  done
  # The Windows and OpenPGP parts stay in every one.
  [[ "$text" == *'the installers are not code-signed yet, so SmartScreen warns'* ]] || fail "$version lacks the Windows note"
  [[ "$text" == *'gpg --verify SHA256SUMS.asc SHA256SUMS'* ]] || fail "$version lacks the OpenPGP check"
  [[ "$(grep -c '^```' <<< "$text")" == 2 ]] || fail "$version's notes have $(grep -c '^```' <<< "$text") fences"
done
# shellcheck disable=SC2016 # the backticks are Markdown
[[ "$(notes_for 0.11.0)" == *'but the OpenPGP signatures (`.asc`) and `appcast.xml`.'* ]] \
  || fail "0.11.0's SHA256SUMS line does not leave out appcast.xml"

# Without GITHUB_REPOSITORY, the key link names this repository.
default="$(env -u GITHUB_REPOSITORY "$script" 0.2.0 v0.2.0 "$dir" "$public_key")"
[[ "$default" == *'https://raw.githubusercontent.com/NicolaiSchmid/steno/v0.2.0/'* ]] \
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
refuse 'a file that is no key' 0.2.0 v0.2.0 "$dir" "$scratch/garbage.asc"
unsigned="$scratch/unsigned"
mkdir -p "$unsigned"
printf 'bytes\n' > "$unsigned/SHA256SUMS"
refuse 'a directory without SHA256SUMS.asc' 0.2.0 v0.2.0 "$unsigned" "$public_key"

if ((failures > 0)); then
  echo "release-notes: $failures failed"
  exit 1
fi
echo "release-notes: ok"
