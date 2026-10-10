#!/usr/bin/env bash
# Bumps the AUR package `steno-desktop-bin` (packaging/aur, stable plan X6)
# to a stable release and pushes it to the AUR: Bump steps 1, 2 and 4 of
# packaging/aur/README.md without makepkg, then the push its "Push to the
# AUR" section describes. desktop-release.yml's `publish` runs it for a
# stable release once AUR_SSH_PRIVATE_KEY exists, and then opens a pull
# request with the same change to packaging/aur.
#
#   apps/desktop/scripts/aur-bump.sh <version> <assets dir>
#
# - <version> is X.Y.Z: candidates are never pushed (D6, X6).
# - PKGBUILD: `pkgver` <version>, `pkgrel` 1, and the first two
#   `sha256sums`, the .deb's and its .asc's, from the files in <assets dir>.
# - .SRCINFO: the same in its own lines, `provides` and the two release
#   URLs with them, so it stays what `makepkg --printsrcinfo` prints
#   (aur-ci.yml checks that on the pull request).
# - Fails without changing the push target when the PKGBUILD is not in the
#   form above (a third release source, the 0.1.0 pin's drop-in copies that
#   Bump step 3 deletes) or a rewrite did not take.
# - The push: a clone of the AUR repository, its files replaced by
#   PKGBUILD, .SRCINFO, .gitignore, keys/ and every local source, one
#   commit `steno-desktop-bin <version>-1`, pushed to `master`.
#
# Environment: AUR_SSH_PRIVATE_KEY, the AUR account's SSH key. AUR_URL
# replaces the AUR's clone URL (and the key is then not needed); the
# tests point it at a local bare repository. STENO_REPO_ROOT is the
# checkout whose packaging/aur is bumped (default: this one).
# apps/desktop/scripts/aur-bump.test.sh checks it; rust-ci.yml runs that.
set -euo pipefail

version="${1:?version, e.g. 0.11.1}"
assets="${2:?directory holding the release assets}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="${STENO_REPO_ROOT:-$(git -C "$here" rev-parse --show-toplevel)}"
aur="$repo_root/packaging/aur"
release="https://github.com/NicolaiSchmid/steno/releases/download/v$version"
deb="steno-desktop_${version}_amd64.deb"
# The AUR's Ed25519 host key, as aur.archlinux.org publishes it
# (SHA256:RFzBCUItH9LZS0cKB5UE6ceAYhBD5C8GeOBip8Z11+4).
host_key='aur.archlinux.org ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEuBKrPzbawxA/k2g6NcyV5jmqwJ2s+zpgZGZ7tpLIcN'

die() {
  echo "::error::$*" >&2
  exit 1
}

[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "the AUR takes stable releases only, not $version"
[[ -n "${AUR_URL:-}" || -n "${AUR_SSH_PRIVATE_KEY:-}" ]] || die "AUR_SSH_PRIVATE_KEY is not set"
for file in "$deb" "$deb.asc"; do
  [[ -f "$assets/$file" ]] || die "$assets has no $file"
done
sha() { sha256sum "$1" | cut -d' ' -f1; }
deb_sha="$(sha "$assets/$deb")"
asc_sha="$(sha "$assets/$deb.asc")"

# The form the rewrite expects: the two release files first in source=,
# then local files only.
sources="$(sed -n 's/^\tsource = //p' "$aur/.SRCINFO")"
[[ "$(grep -c '://' <<< "$sources")" == 2 && "$(head -n 2 <<< "$sources" | grep -c '://')" == 2 ]] \
  || die "packaging/aur/.SRCINFO does not list the .deb and its .asc first and nothing else remote: $sources"
if grep -q '^ *_copy ' "$aur/PKGBUILD"; then
  die "packaging/aur/PKGBUILD still installs drop-in copies; the release .deb ships them (README, Bump step 3)"
fi

work="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/aur-bump.XXXXXX")"
trap 'rm -rf "$work"' EXIT

# PKGBUILD: pkgver, pkgrel and the first two entries of sha256sums.
awk -v version="$version" -v deb="$deb_sha" -v asc="$asc_sha" '
  /^pkgver=/ { print "pkgver=" version; next }
  /^pkgrel=/ { print "pkgrel=1"; next }
  /^sha256sums=\(/ { in_sums = 1 }
  in_sums && n < 2 && match($0, /'\''[0-9a-f]+'\''/) {
    $0 = substr($0, 1, RSTART) (n == 0 ? deb : asc) substr($0, RSTART + RLENGTH - 1)
    n++
  }
  in_sums && /\)/ { in_sums = 0 }
  { print }
' "$aur/PKGBUILD" > "$work/PKGBUILD"
# .SRCINFO: the same, the release URLs and provides.
awk -v version="$version" -v deb="$deb_sha" -v asc="$asc_sha" -v release="$release" -v file="$deb" '
  /^\tpkgver = / { print "\tpkgver = " version; next }
  /^\tpkgrel = / { print "\tpkgrel = 1"; next }
  /^\tprovides = steno-desktop=/ { print "\tprovides = steno-desktop=" version; next }
  /^\tsource = .*:\/\// { print "\tsource = " release "/" file (sources++ == 0 ? "" : ".asc"); next }
  /^\tsha256sums = / && sums < 2 { print "\tsha256sums = " (sums++ == 0 ? deb : asc); next }
  { print }
' "$aur/.SRCINFO" > "$work/.SRCINFO"

# Every rewrite took.
expect() { grep -qxF -- "$2" "$work/$1" || die "the bump left no line '$2' in $1"; }
expect PKGBUILD "pkgver=$version"
expect PKGBUILD 'pkgrel=1'
expect PKGBUILD "sha256sums=('$deb_sha'"
expect PKGBUILD "            '$asc_sha'"
expect .SRCINFO $'\t'"pkgver = $version"
expect .SRCINFO $'\t'"pkgrel = 1"
expect .SRCINFO $'\t'"provides = steno-desktop=$version"
expect .SRCINFO $'\t'"source = $release/$deb"
expect .SRCINFO $'\t'"source = $release/$deb.asc"
[[ "$(sed -n 's/^\tsha256sums = //p' "$work/.SRCINFO" | head -n 2 | tr '\n' ' ')" == "$deb_sha $asc_sha " ]] \
  || die "the bump did not put the .deb's and the .asc's checksums first in .SRCINFO"
cp "$work/PKGBUILD" "$work/.SRCINFO" "$aur/"
echo "packaging/aur is at $version"

# The push.
url="${AUR_URL:-}"
if [[ -z "$url" ]]; then
  (umask 077 && printf '%s\n' "$AUR_SSH_PRIVATE_KEY" > "$work/key" && printf '%s\n' "$host_key" > "$work/known_hosts")
  export GIT_SSH_COMMAND="ssh -i $work/key -o IdentitiesOnly=yes -o UserKnownHostsFile=$work/known_hosts -o StrictHostKeyChecking=yes"
  url=ssh://aur@aur.archlinux.org/steno-desktop-bin.git
fi
git clone --quiet "$url" "$work/aur" 2>/dev/null || die "could not clone the AUR repository"
git -C "$work/aur" rm -rq --ignore-unmatch . >/dev/null
locals=()
while IFS= read -r source; do
  [[ "$source" == *://* ]] || locals+=("$source")
done <<< "$sources"
(cd "$aur" && cp -R PKGBUILD .SRCINFO .gitignore keys "${locals[@]}" "$work/aur/")
git -C "$work/aur" add -A
if git -C "$work/aur" diff --cached --quiet; then
  echo "the AUR already has steno-desktop-bin $version-1"
  exit 0
fi
git -C "$work/aur" \
  -c user.name='github-actions[bot]' \
  -c user.email='41898282+github-actions[bot]@users.noreply.github.com' \
  commit --quiet -m "steno-desktop-bin $version-1"
git -C "$work/aur" push --quiet origin HEAD:master
echo "pushed steno-desktop-bin $version-1 to the AUR"
