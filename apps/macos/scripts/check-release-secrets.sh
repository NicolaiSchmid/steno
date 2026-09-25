#!/usr/bin/env bash
# The release workflow's first step: fail with the names of the missing
# repository secrets before anything is built, instead of a cryptic codesign
# or notarytool error twenty minutes in. A dry run (`DRY_RUN=true`) needs
# only the signing certificate; a real release needs all six.
#
# Reads from the environment (release.yml maps the secrets onto these):
#   P12, P12_PASSWORD              MACOS_CERTIFICATE_P12_BASE64, MACOS_CERTIFICATE_PASSWORD
#   ASC_KEY_ID, ASC_ISSUER_ID, ASC_PRIVATE_KEY
#   SPARKLE_PRIVATE_KEY
#   DRY_RUN                        "true" for a dry run; anything else is a release
# Exit 1 and `::error::missing repository secrets: <names>` when any is empty.
# Runs under /bin/bash 3.2 (no `set -u`: an empty array trips it there).
set -eo pipefail

missing=()
[ -n "${P12:-}" ] || missing+=(MACOS_CERTIFICATE_P12_BASE64)
[ -n "${P12_PASSWORD:-}" ] || missing+=(MACOS_CERTIFICATE_PASSWORD)
if [ "${DRY_RUN:-false}" != "true" ]; then
  [ -n "${ASC_KEY_ID:-}" ] || missing+=(ASC_KEY_ID)
  [ -n "${ASC_ISSUER_ID:-}" ] || missing+=(ASC_ISSUER_ID)
  [ -n "${ASC_PRIVATE_KEY:-}" ] || missing+=(ASC_PRIVATE_KEY)
  [ -n "${SPARKLE_PRIVATE_KEY:-}" ] || missing+=(SPARKLE_PRIVATE_KEY)
fi

if [ "${#missing[@]}" -gt 0 ]; then
  echo "::error::missing repository secrets: ${missing[*]}. Create the Developer ID Application certificate, export it as .p12, base64-encode it into MACOS_CERTIFICATE_P12_BASE64 and put its password in MACOS_CERTIFICATE_PASSWORD (apps/macos/README.md, 'One-time setup')."
  exit 1
fi
echo "all release secrets present (dry run: ${DRY_RUN:-false})"
