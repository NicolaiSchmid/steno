#!/usr/bin/env bash
# The Developer ID certificate in a throwaway keychain for one release run,
# as the Swift app's release workflow did: `import` creates
# and unlocks the keychain, imports the .p12 with a partition list that lets
# codesign use the key without a prompt, puts the keychain in front of the
# user's keychain search list, then prints the SHA-1 of the Developer ID
# Application identity (what the Tauri bundler signs with,
# `APPLE_SIGNING_IDENTITY`) as the only line on stdout; every tool's own
# output goes to stderr. `remove` deletes the keychain and takes it out of
# the search list as it is then, so another run's keychain on the
# self-hosted Mac stays and the user's own keychains are never touched.
#
#   P12=<base64 .p12> P12_PASSWORD=<password> \
#     apps/desktop/scripts/signing-keychain.sh import <keychain>
#   apps/desktop/scripts/signing-keychain.sh remove <keychain>
#
# The keychain password is random and lives only for the run. Paths with a
# trailing space corrupt `security list-keychains -s`, so callers pass
# plain paths; the keychain's directory must exist.
set -euo pipefail

action="${1:?import or remove}"
keychain="${2:?keychain path}"
# The search list holds the resolved path (no `..`, no symlink), so this
# one is compared in that form.
keychain="$(cd "$(dirname "$keychain")" && pwd -P)/$(basename "$keychain")"

# The search list as it is now, minus this run's keychain, into `others`;
# `listed` says whether the keychain was on it.
read_list() {
  local list line
  list="$(security list-keychains -d user | sed 's/^ *"//; s/"$//')"
  others=()
  listed=false
  while IFS= read -r line; do
    if [[ "$line" == "$keychain" ]]; then
      listed=true
    elif [[ -n "$line" ]]; then
      others+=("$line")
    fi
  done <<< "$list"
}

case "$action" in
  import)
    : "${P12:?P12 (the base64 .p12) missing}"
    : "${P12_PASSWORD:?P12_PASSWORD missing}"
    # Stdout is the identity alone (`security import` reports what it
    # imported there), so fd 3 keeps it and everything else goes to stderr.
    exec 3>&1 1>&2
    password="$(openssl rand -hex 24)"
    certificate="$(mktemp)"
    trap 'rm -f "$certificate"' EXIT
    base64 --decode <<< "$P12" > "$certificate"
    # A runner lost mid-run leaves its keychain file behind.
    security delete-keychain "$keychain" 2>/dev/null || true
    security create-keychain -p "$password" "$keychain"
    security set-keychain-settings -lut 21600 "$keychain"
    security unlock-keychain -p "$password" "$keychain"
    security import "$certificate" -P "$P12_PASSWORD" -A -t cert -f pkcs12 -k "$keychain"
    security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$password" "$keychain" >/dev/null
    read_list
    # `${others[@]+…}`: an empty array under `set -u` is an error in bash 3.2.
    security list-keychains -d user -s "$keychain" ${others[@]+"${others[@]}"}
    identity="$(security find-identity -v -p codesigning "$keychain" \
      | sed -n 's/^ *[0-9]*) \([0-9A-F]\{40\}\) "Developer ID Application: .*"$/\1/p' | head -n 1)"
    [[ -n "$identity" ]] || { echo "::error::the imported certificate is not a Developer ID Application identity"; exit 1; }
    echo "$identity" >&3
    ;;
  remove)
    # `delete-keychain` also takes it off the search list; the list is
    # rewritten only if delete-keychain leaves the entry.
    security delete-keychain "$keychain" 2>/dev/null || true
    read_list
    if [[ "$listed" == true ]]; then
      security list-keychains -d user -s ${others[@]+"${others[@]}"} 2>/dev/null || true
    fi
    ;;
  *)
    echo "::error::unknown action \"$action\" (import or remove)" >&2
    exit 1
    ;;
esac
