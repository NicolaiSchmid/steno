#!/usr/bin/env bash
# The notes of a desktop release (Markdown on stdout): what it carries, that
# the Windows installers are not code-signed, and how to check a download
# against `SHA256SUMS` and the OpenPGP signatures release-signatures.sh
# wrote into <dir>, naming the signing key's fingerprint from <public key>.
#
#   apps/desktop/scripts/release-notes.sh <version> <tag> <dir> apps/desktop/release-signing-key.asc
#
# The public key's link points at the copy in the repository at <tag>, in
# $GITHUB_REPOSITORY (NicolaiSchmid/steno when unset).
# apps/desktop/scripts/release-notes.test.sh checks it; rust-ci.yml runs that.
set -euo pipefail

version="${1:?version, e.g. 0.11.0}"
tag="${2:?the release tag, e.g. desktop-v0.11.0}"
dir="${3:?directory holding the release assets}"
public_key="${4:?the committed public key (armored)}"
repository="${GITHUB_REPOSITORY:-NicolaiSchmid/steno}"

home="$(mktemp -d)"
trap 'GNUPGHOME="$home" gpgconf --kill all 2>/dev/null || true; rm -rf "$home"' EXIT
fingerprint="$({ GNUPGHOME="$home" gpg --batch --with-colons --show-keys "$public_key" 2>/dev/null || true; } \
  | awk -F: '$1 == "fpr" { print $10; exit }')"
[[ "$fingerprint" =~ ^[0-9A-F]{40}$ ]] \
  || { echo "::error::$public_key holds no OpenPGP public key" >&2; exit 1; }
[[ -f "$dir/SHA256SUMS" && -f "$dir/SHA256SUMS.asc" ]] \
  || { echo "::error::$dir has no SHA256SUMS and SHA256SUMS.asc; run release-signatures.sh first" >&2; exit 1; }

# The signed bundles, as `gpg --verify` lines.
bundles=()
while IFS= read -r -d '' signature; do
  file="${signature#"$dir"/}"
  file="${file%.asc}"
  [[ "$file" == SHA256SUMS ]] || bundles+=("gpg --verify $file.asc $file")
done < <(find "$dir" -maxdepth 1 -type f -name '*.asc' -print0 | LC_ALL=C sort -z)

# The fingerprint as gpg prints it: ten groups of four, two spaces between
# the halves.
half() {
  sed -E 's/(.{4})(.{4})(.{4})(.{4})(.{4})/\1 \2 \3 \4 \5/' <<< "$1"
}
grouped="$(half "${fingerprint:0:20}")  $(half "${fingerprint:20}")"

key_url="https://raw.githubusercontent.com/$repository/$tag/apps/desktop/release-signing-key.asc"
fence='```'

cat <<EOF
Steno desktop $version for macOS (Apple silicon, signed and notarised), Linux (\`.deb\`, AppImage) and Windows (\`.msi\`, NSIS \`-setup.exe\`). On macOS, install the Mac app from the Swift releases (the repository's latest release) for now.

**Windows:** the installers are not code-signed yet, so SmartScreen warns before the first install ("Windows protected your PC"; choose *More info*, then *Run anyway*). Check the installer against \`SHA256SUMS\` first: in PowerShell, \`(Get-FileHash .\\<installer>).Hash -eq '<its hash in SHA256SUMS>'\` must print \`True\`.

### Verify a download

\`SHA256SUMS\` lists every file of this release but the signatures. It and each Linux bundle have a detached OpenPGP signature (\`<file>.asc\`) from the Steno release signing key:

    $grouped

Download \`SHA256SUMS\`, \`SHA256SUMS.asc\` and the files you want into one directory, then run these commands in it (where \`curl\` is missing, \`wget\` fetches the key as well):

${fence}sh
curl -fsSLO $key_url
gpg --import release-signing-key.asc
gpg --verify SHA256SUMS.asc SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS
EOF
if [[ ${#bundles[@]} -gt 0 ]]; then printf '%s\n' "${bundles[@]}"; fi
cat <<EOF
$fence

Each \`gpg --verify\` must say "Good signature" from the fingerprint above (the warning that the key is not certified is expected), and \`sha256sum\` must print OK for every file you downloaded. The \`.sig\` files are the in-app updater's signatures, not OpenPGP ones.
EOF
