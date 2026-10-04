#!/usr/bin/env bash
# The Developer ID certificate in a throwaway keychain for one release run,
# as .github/workflows/release.yml does for the Swift app: `import` creates
# and unlocks the keychain, imports the .p12 with a partition list that lets
# codesign use the key without a prompt, saves the user's keychain search
# list and puts the keychain in front of it, then prints the SHA-1 of the
# Developer ID Application identity (what the Tauri bundler signs with,
# `APPLE_SIGNING_IDENTITY`). `remove` deletes the keychain and puts the
# saved search list back, never a guess at it: on the self-hosted Mac the
# runner user may keep other keychains there.
#
#   P12=<base64 .p12> P12_PASSWORD=<password> signing-keychain.sh import <keychain> <saved list>
#   signing-keychain.sh remove <keychain> <saved list>
#
# The keychain password is random and lives only for the run. Paths with a
# trailing space corrupt `security list-keychains -s`, so callers pass
# plain paths.
set -euo pipefail

action="${1:?import or remove}"
keychain="${2:?keychain path}"
saved="${3:?file the search list is saved to}"

# The saved search list as an array, one keychain per line.
read_saved() {
  before=()
  while IFS= read -r line; do
    [[ -n "$line" ]] && before+=("$line")
  done < "$saved"
}

case "$action" in
  import)
    : "${P12:?P12 (the base64 .p12) missing}"
    : "${P12_PASSWORD:?P12_PASSWORD missing}"
    password="$(openssl rand -hex 24)"
    certificate="$(mktemp)"
    trap 'rm -f "$certificate"' EXIT
    base64 --decode <<< "$P12" > "$certificate"
    security create-keychain -p "$password" "$keychain"
    security set-keychain-settings -lut 21600 "$keychain"
    security unlock-keychain -p "$password" "$keychain"
    security import "$certificate" -P "$P12_PASSWORD" -A -t cert -f pkcs12 -k "$keychain"
    security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$password" "$keychain" >/dev/null
    security list-keychains -d user | sed 's/^ *"//; s/"$//' > "$saved"
    read_saved
    security list-keychains -d user -s "$keychain" "${before[@]}"
    identity="$(security find-identity -v -p codesigning "$keychain" \
      | sed -n 's/^ *[0-9]*) \([0-9A-F]\{40\}\) "Developer ID Application: .*"$/\1/p' | head -n 1)"
    [[ -n "$identity" ]] || { echo "::error::the imported certificate is not a Developer ID Application identity" >&2; exit 1; }
    echo "$identity"
    ;;
  remove)
    security delete-keychain "$keychain" 2>/dev/null || true
    if [[ -s "$saved" ]]; then
      read_saved
      security list-keychains -d user -s "${before[@]}" 2>/dev/null || true
      rm -f "$saved"
    fi
    ;;
  *)
    echo "::error::unknown action \"$action\" (import or remove)" >&2
    exit 1
    ;;
esac
