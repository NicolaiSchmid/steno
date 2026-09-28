#!/usr/bin/env bash
# Folds this release's appcast item into the rolling appcast on the
# `appcast` branch, which `SUFeedURL` points at
# (https://raw.githubusercontent.com/NicolaiSchmid/steno/appcast/appcast.xml).
# Runs after the GitHub release is public so the enclosure URL resolves.
# Pre-releases (tag with a hyphen) are tagged with the `beta` channel, which
# only release-candidate builds read (UpdateChannels.swift).
#
# Usage: publish-appcast.sh <tag> <prerelease: true|false>
# Environment (overrides for tests):
#   STENO_REPO_ROOT       the checkout whose `origin` receives the push
#   STENO_RELEASE_APPCAST the per-release appcast (default apps/macos/dist/appcast.xml)
#   APPCAST_BRANCH        the branch name (default appcast)
set -euo pipefail

tag="${1:?release tag, e.g. v1.2.3}"
prerelease="${2:?prerelease true|false}"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
app_dir="$(cd "$here/.." && pwd)"
repo_root="${STENO_REPO_ROOT:-$(git -C "$app_dir" rev-parse --show-toplevel)}"
release_appcast="${STENO_RELEASE_APPCAST:-$app_dir/dist/appcast.xml}"
branch="${APPCAST_BRANCH:-appcast}"
merge="$here/merge-appcast.py"

test -s "$release_appcast" || { echo "::error::release appcast missing at $release_appcast"; exit 1; }
grep -q 'sparkle:edSignature' "$release_appcast" || { echo "::error::release appcast carries no EdDSA signature"; exit 1; }

work="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/steno-appcast-branch-$$"
cleanup() { git -C "$repo_root" worktree remove --force "$work" 2>/dev/null || rm -rf "$work"; }
trap cleanup EXIT

if git -C "$repo_root" fetch --quiet origin "refs/heads/$branch" 2>/dev/null; then
  echo "==> $branch exists; updating"
  git -C "$repo_root" worktree add --quiet --detach "$work" FETCH_HEAD
else
  echo "==> $branch does not exist yet; creating it"
  git -C "$repo_root" worktree add --quiet --detach "$work" HEAD
  git -C "$work" checkout --quiet --orphan "$branch"
  git -C "$work" rm -rfq . >/dev/null 2>&1 || true
  git -C "$work" clean -fdxq
  printf '# Steno update feed\n\nSparkle reads `appcast.xml` from this branch. It is written by the release workflow; do not edit by hand.\n' > "$work/README.md"
fi

channel_args=()
if [ "$prerelease" = "true" ]; then channel_args=(--channel beta); fi
# `${arr[@]+"${arr[@]}"}`: an empty array under `set -u` is an error in bash 3.2.
python3 "$merge" "$work/appcast.xml" "$release_appcast" "$work/appcast.xml" ${channel_args[@]+"${channel_args[@]}"}

git -C "$work" add --all
if git -C "$work" diff --cached --quiet; then
  echo "appcast unchanged for $tag"
  exit 0
fi
git -C "$work" \
  -c user.name="${GIT_AUTHOR_NAME:-github-actions[bot]}" \
  -c user.email="${GIT_AUTHOR_EMAIL:-41898282+github-actions[bot]@users.noreply.github.com}" \
  commit --quiet -m "chore(release): appcast for $tag"
git -C "$work" push --quiet origin "HEAD:refs/heads/$branch"
echo "published $tag to $branch"
