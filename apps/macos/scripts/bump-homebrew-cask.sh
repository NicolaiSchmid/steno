#!/usr/bin/env bash
# Points Casks/steno.rb in the Homebrew tap (NicolaiSchmid/homebrew-tap) at
# the release the workflow just published: `version` becomes <version>,
# `sha256` the SHA-256 of the DMG that was uploaded. desktop-release.yml's
# `publish` runs it for stable releases only; candidates never bump the
# cask (D7 of .plans/2026-10-07-stable-promotion.md).
#
# Usage: bump-homebrew-cask.sh <version> <dmg>
#        e.g. bump-homebrew-cask.sh 1.2.3 assets/Steno_1.2.3_aarch64.dmg
# Environment:
#   HOMEBREW_TAP_TOKEN  fine-grained PAT with Contents read and write on the
#                       tap. Empty and no HOMEBREW_TAP_URL: print a notice
#                       and exit 0; the release is published by then and
#                       must not fail over an optional step.
#   HOMEBREW_TAP_URL    clone URL, default the tap over HTTPS with the token.
#                       ReleaseScriptsTests point it at a local bare repo.
# Runs on the Linux runner in `publish`, and under /bin/bash 3.2 in
# ReleaseScriptsTests on macOS. apps/desktop/scripts/bump-homebrew-cask.test.sh
# checks the token check on Linux.
set -euo pipefail

version="${1:?version, e.g. 1.2.3}"
dmg="${2:?path to Steno-<version>.dmg}"

if [ -z "${HOMEBREW_TAP_URL:-}" ]; then
  if [ -z "${HOMEBREW_TAP_TOKEN:-}" ]; then
    echo "::notice::HOMEBREW_TAP_TOKEN is not set; Casks/steno.rb in NicolaiSchmid/homebrew-tap stays where it is. Create the token as apps/desktop/README.md (Secrets) describes to bump it from here."
    exit 0
  fi
  HOMEBREW_TAP_URL="https://x-access-token:${HOMEBREW_TAP_TOKEN}@github.com/NicolaiSchmid/homebrew-tap.git"
fi

[ -f "$dmg" ] || { echo "::error::$dmg not found; nothing to hash for the cask"; exit 1; }
sha="$(shasum -a 256 "$dmg" | cut -d' ' -f1)"
[ "${#sha}" -eq 64 ] || { echo "::error::could not hash $dmg"; exit 1; }

work="$(mktemp -d "${TMPDIR:-/tmp}/homebrew-tap.XXXXXX")"
trap 'rm -rf "$work"' EXIT
# The token sits in the URL; keep git quiet so it never reaches the log
# unmasked, and never echo the URL.
git clone --quiet --depth 1 --branch main "$HOMEBREW_TAP_URL" "$work/tap"
cask="$work/tap/Casks/steno.rb"
[ -f "$cask" ] || { echo "::error::Casks/steno.rb missing from the tap"; exit 1; }
{ grep -q '^  version "' "$cask" && grep -q '^  sha256 "' "$cask"; } \
  || { echo "::error::Casks/steno.rb has no version/sha256 stanza in the expected form"; exit 1; }

sed -e "s|^  version \".*\"|  version \"$version\"|" \
  -e "s|^  sha256 \".*\"|  sha256 \"$sha\"|" "$cask" > "$cask.new"
mv "$cask.new" "$cask"
{ grep -q "^  version \"$version\"$" "$cask" && grep -q "^  sha256 \"$sha\"$" "$cask"; } \
  || { echo "::error::rewriting Casks/steno.rb did not take"; exit 1; }

cd "$work/tap"
if git diff --quiet; then
  echo "Casks/steno.rb already at $version ($sha)"
  exit 0
fi
git -c user.name='github-actions[bot]' \
  -c user.email='41898282+github-actions[bot]@users.noreply.github.com' \
  commit --quiet --all --message "steno $version"
git push --quiet origin HEAD:main
echo "Casks/steno.rb bumped to $version ($sha)"
