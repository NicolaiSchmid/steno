#!/usr/bin/env bash
# Signs the Sparkle handoff item for one stable release: the one item
# without a channel through which every Swift build updates to this app
# (stable plan D8, "The Sparkle handoff"). desktop-release.yml's macOS job
# runs it on tag runs only, after the disk image is notarised and stapled,
# since the EdDSA signature covers the image's bytes.
#
#   apps/desktop/scripts/handoff-item.sh <version> <build> <dmg> <out dir>
#
# - `generate_appcast` comes from the Sparkle 2.10.0 release tarball, its
#   SHA-256 pinned below; the key comes from SPARKLE_PRIVATE_KEY on stdin
#   (`--ed-key-file -`), never from a file or the keychain.
# - The item points at `releases/download/v<version>/<dmg name>`, has no
#   delta (`--maximum-deltas 0`), and rolls out in seven groups a day apart
#   (`--phased-rollout-interval 86400`).
# - handoff-appcast.py's `check-item` then requires exactly that item, its
#   `sparkle:edSignature` included: `generate_appcast` skips signing
#   without an error when the key does not match the app's SUPublicEDKey.
# - Last, a dry run of apps/macos/scripts/merge-appcast.py folds the item
#   into the `appcast` branch's feed as the `handoff` job will, and the
#   build must be above every build there.
#
# Writes <out dir>/appcast.xml, the `sparkle-item` artifact's one file.
# Environment: SPARKLE_PRIVATE_KEY (base64, from `generate_keys -x`);
# GITHUB_REPOSITORY (NicolaiSchmid/steno when unset); STENO_REPO_ROOT, the
# checkout whose `origin` holds the `appcast` branch (default: this one).
# For the tests: STENO_GENERATE_APPCAST runs that program instead of the
# tarball's, and STENO_SPARKLE_URL fetches the tarball from elsewhere.
# apps/desktop/scripts/handoff-item.test.sh checks it; rust-ci.yml runs that.
set -euo pipefail

version="${1:?version, e.g. 0.11.0}"
build="${2:?build number, the commit count}"
dmg="${3:?the notarised disk image}"
out="${4:?directory for appcast.xml}"
: "${SPARKLE_PRIVATE_KEY:?SPARKLE_PRIVATE_KEY missing}"

sparkle_version=2.10.0
sparkle_sha256=c2bf58aa8387266ac179357b1415d6f2635f044da8be41042af32425dae6da0c
sparkle_url="${STENO_SPARKLE_URL:-https://github.com/sparkle-project/Sparkle/releases/download/$sparkle_version/Sparkle-$sparkle_version.tar.xz}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="${STENO_REPO_ROOT:-$(git -C "$here" rev-parse --show-toplevel)}"
repo="https://github.com/${GITHUB_REPOSITORY:-NicolaiSchmid/steno}"
prefix="$repo/releases/download/v$version/"

die() {
  echo "::error::$*" >&2
  exit 1
}

[[ -f "$dmg" ]] || die "$dmg is not a file"
work="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/handoff-item.XXXXXX")"
trap 'rm -rf "$work"' EXIT

generate_appcast="${STENO_GENERATE_APPCAST:-}"
if [[ -z "$generate_appcast" ]]; then
  curl -fsSL --retry 3 -o "$work/sparkle.tar.xz" "$sparkle_url" || die "could not download $sparkle_url"
  if command -v sha256sum >/dev/null; then
    sum="$(sha256sum "$work/sparkle.tar.xz" | cut -d' ' -f1)"
  else
    sum="$(shasum -a 256 "$work/sparkle.tar.xz" | cut -d' ' -f1)"
  fi
  [[ "$sum" == "$sparkle_sha256" ]] \
    || die "Sparkle-$sparkle_version.tar.xz has the SHA-256 $sum, expected $sparkle_sha256"
  mkdir -p "$work/sparkle"
  tar -xJf "$work/sparkle.tar.xz" -C "$work/sparkle" ./bin
  generate_appcast="$work/sparkle/bin/generate_appcast"
fi

# Only the image may sit in the folder generate_appcast scans.
mkdir -p "$work/archives" "$out"
cp "$dmg" "$work/archives/"
printf '%s' "$SPARKLE_PRIVATE_KEY" | "$generate_appcast" \
  --ed-key-file - \
  --download-url-prefix "$prefix" \
  --link "$repo/releases" \
  --maximum-deltas 0 \
  --phased-rollout-interval 86400 \
  -o "$out/appcast.xml" \
  "$work/archives"

python3 "$here/handoff-appcast.py" check-item "$out/appcast.xml" "$version" "$build" "$prefix${dmg##*/}"

git -C "$repo_root" fetch --quiet origin refs/heads/appcast \
  || die "could not fetch the appcast branch from origin"
git -C "$repo_root" show FETCH_HEAD:appcast.xml > "$work/rolling.xml"
python3 "$here/handoff-appcast.py" check-build "$work/rolling.xml" "$build"
python3 "$here/../../macos/scripts/merge-appcast.py" "$work/rolling.xml" "$out/appcast.xml" "$work/merged.xml"
echo "$out/appcast.xml"
