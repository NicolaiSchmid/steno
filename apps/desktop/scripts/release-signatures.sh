#!/usr/bin/env bash
# The release's checksums and OpenPGP signatures, written into the release
# directory: `SHA256SUMS` over every asset in it, then a detached armored
# signature (`<file>.asc`) for `SHA256SUMS` and for each Linux `.deb` and
# `.AppImage`. Each signature is then verified against the public key
# committed at <public key>, with nothing else in the keyring; a mismatch is
# an `::error::` and exit 1, so a secret key that is not that key's other
# half never ships.
#
#   GPG_PRIVATE_KEY=<armored secret key> GPG_PASSPHRASE=<its passphrase> \
#     apps/desktop/scripts/release-signatures.sh <dir> apps/desktop/release-signing-key.asc
#
# The secret key goes into a throwaway GNUPGHOME under $RUNNER_TEMP (or
# $TMPDIR), read from the environment and never printed; the passphrase
# reaches gpg through a file in that directory (loopback pinentry). The
# directory and its agent are removed on exit, failed or not. Prints the
# signing key's fingerprint as the last line on stdout.
# apps/desktop/scripts/release-signatures.test.sh checks it with a test
# key; rust-ci.yml runs that.
set -euo pipefail

dir="${1:?directory holding the release assets}"
public_key="${2:?the committed public key (armored)}"
: "${GPG_PRIVATE_KEY:?GPG_PRIVATE_KEY (the armored secret key) missing}"
: "${GPG_PASSPHRASE:?GPG_PASSPHRASE missing}"
[[ -d "$dir" ]] || { echo "::error::$dir is not a directory" >&2; exit 1; }
[[ -f "$public_key" ]] || { echo "::error::no public key at $public_key" >&2; exit 1; }
public_key="$(cd "$(dirname "$public_key")" && pwd)/$(basename "$public_key")"

scratch="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/steno-gnupg.XXXXXX")"
signing="$scratch/signing"
verifying="$scratch/verifying"
cleanup() {
  local home
  for home in "$signing" "$verifying"; do
    [[ -d "$home" ]] && GNUPGHOME="$home" gpgconf --kill all 2>/dev/null || true
  done
  rm -rf "$scratch"
}
trap cleanup EXIT
(umask 077 && mkdir "$signing" "$verifying")

# key_field <record> <field>: one field of the committed key's first
# <record> line in gpg's colon listing, empty when gpg cannot read the file.
key_field() {
  { GNUPGHOME="$verifying" gpg --batch --with-colons --show-keys "$public_key" 2>/dev/null || true; } \
    | awk -F: -v record="$1" -v field="$2" '$1 == record { print $field; exit }'
}

# The committed key's fingerprint: the only key signatures may come from.
fingerprint="$(key_field fpr 10)"
[[ "$fingerprint" =~ ^[0-9A-F]{40}$ ]] \
  || { echo "::error::$public_key holds no OpenPGP public key" >&2; exit 1; }
keyring="$verifying/release-signing-key.gpg"
GNUPGHOME="$verifying" gpg --batch --dearmor < "$public_key" > "$keyring"

(umask 077 && printf '%s' "$GPG_PASSPHRASE" > "$signing/passphrase")
gpg_sign() {
  GNUPGHOME="$signing" gpg --batch --no-tty --pinentry-mode loopback \
    --passphrase-file "$signing/passphrase" "$@"
}
printf '%s\n' "$GPG_PRIVATE_KEY" | gpg_sign --quiet --import \
  || { echo "::error::GPG_PRIVATE_KEY is not an OpenPGP secret key gpg can import" >&2; exit 1; }

cd "$dir"
# Every asset but the files this script writes, in a stable order.
assets=()
while IFS= read -r -d '' file; do
  assets+=("${file#./}")
done < <(find . -maxdepth 1 -type f ! -name SHA256SUMS ! -name '*.asc' -print0 | LC_ALL=C sort -z)
[[ ${#assets[@]} -gt 0 ]] || { echo "::error::no assets in $dir" >&2; exit 1; }
rm -f SHA256SUMS ./*.asc
sha256sum -- "${assets[@]}" > SHA256SUMS

signed=(SHA256SUMS)
for file in "${assets[@]}"; do
  case "$file" in *.deb | *.AppImage) signed+=("$file") ;; esac
done
[[ ${#signed[@]} -gt 1 ]] || echo "::warning::no .deb or .AppImage in $dir; only SHA256SUMS is signed" >&2

# Each file signed, then checked with gpgv, which trusts exactly the keys
# in --keyring: the committed one.
for file in "${signed[@]}"; do
  gpg_sign --quiet --local-user "$fingerprint!" --armor --detach-sign --output "$file.asc" -- "$file" \
    || { echo "::error::could not sign $file with $fingerprint; is GPG_PRIVATE_KEY the secret half of $public_key, and GPG_PASSPHRASE its passphrase?" >&2; exit 1; }
  status="$(GNUPGHOME="$verifying" gpgv --keyring "$keyring" --status-fd 1 -- "$file.asc" "$file" || true)"
  grep -q "^\[GNUPG:\] VALIDSIG $fingerprint " <<< "$status" \
    || { echo "::error::$file.asc does not verify against $public_key" >&2; exit 1; }
  echo "verified $file.asc" >&2
done

# A warning a quarter before the key expires (field 7 of `pub`, empty
# when it never does).
expires="$(key_field pub 7)"
if [[ -n "$expires" ]] && ((expires - $(date +%s) < 90 * 86400)); then
  echo "::warning::the release signing key $fingerprint expires on $(date -u -d "@$expires" +%F); extend it (apps/desktop/README.md, Release)" >&2
fi

echo "signed ${signed[*]} with $fingerprint; SHA256SUMS covers ${#assets[@]} assets" >&2
echo "$fingerprint"
