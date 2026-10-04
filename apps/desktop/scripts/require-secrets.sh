#!/usr/bin/env bash
# The desktop release job's first step: fails naming every repository secret
# in the argument list whose environment variable of the same name is empty,
# before anything is built, instead of a cryptic signing or notarisation
# error later. The workflow maps each secret onto a variable of its name.
#
#   MACOS_CERTIFICATE_PASSWORD=… require-secrets.sh MACOS_CERTIFICATE_PASSWORD ASC_KEY_ID
set -euo pipefail

missing=()
for name in "$@"; do
  [[ -n "${!name:-}" ]] || missing+=("$name")
done
if [[ ${#missing[@]} -gt 0 ]]; then
  echo "::error::missing repository secrets: ${missing[*]} (apps/desktop/README.md, Release)" >&2
  exit 1
fi
echo "release secrets present: $*"
