#!/usr/bin/env bash
# Checks apps/desktop/scripts/release-signatures.sh, the checksums and
# OpenPGP signatures the desktop release workflow publishes, with two
# throwaway ed25519 keys made here; rust-ci.yml runs it on Linux. Needs gpg
# and gpgv.
set -euo pipefail

script="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/release-signatures.sh"
scratch="$(mktemp -d)"
keys="$scratch/keys"
cleanup() {
  GNUPGHOME="$keys" gpgconf --kill all 2>/dev/null || true
  rm -rf "$scratch"
}
trap cleanup EXIT
failures=0

fail() {
  echo "FAIL: $1"
  failures=$((failures + 1))
}

# The release key and an impostor, each with its own passphrase, exported
# as the workflow's secrets hold them.
(umask 077 && mkdir "$keys")
passphrase='correct horse battery staple'
# make_key <uid> <passphrase> [gpg option...]
make_key() {
  GNUPGHOME="$keys" gpg --batch --quiet --pinentry-mode loopback --passphrase "$2" "${@:3}" \
    --quick-generate-key "$1" ed25519 sign 1y
  GNUPGHOME="$keys" gpg --batch --with-colons --list-keys "$1" 2>/dev/null | awk -F: '$1 == "fpr" { print $10; exit }'
}
export_secret() {
  GNUPGHOME="$keys" gpg --batch --pinentry-mode loopback --passphrase "$2" --armor --export-secret-keys "$1"
}
release="$(make_key 'Release test <release@example.invalid>' "$passphrase")"
impostor="$(make_key 'Impostor test <impostor@example.invalid>' other)"
GNUPGHOME="$keys" gpg --batch --armor --export "$release" > "$scratch/release.asc"
GNUPGHOME="$keys" gpg --batch --armor --export "$impostor" > "$scratch/impostor.asc"
release_secret="$(export_secret "$release" "$passphrase")"
impostor_secret="$(export_secret "$impostor" other)"
# A key made in 2020 that expired a year later.
expired="$(make_key 'Expired test <expired@example.invalid>' "$passphrase" --faked-system-time 20200101T000000 2>/dev/null)"
GNUPGHOME="$keys" gpg --batch --armor --export "$expired" > "$scratch/expired.asc"
expired_secret="$(export_secret "$expired" "$passphrase")"

# assets <name>: a release directory as the publish job assembles it.
assets() {
  local dir="$scratch/$1" file
  mkdir -p "$dir"
  for file in Steno_0.2.0_aarch64.app.tar.gz Steno_0.2.0_aarch64.app.tar.gz.sig \
    Steno_0.2.0_aarch64.dmg steno-desktop_0.2.0_amd64.AppImage \
    steno-desktop_0.2.0_amd64.AppImage.sig steno-desktop_0.2.0_amd64.deb \
    steno-desktop_0.2.0_amd64.deb.sig Steno_0.2.0_x64-setup.exe Steno_0.2.0_x64_en-US.msi \
    latest.json; do
    printf 'bytes of %s\n' "$file" > "$dir/$file"
  done
  echo "$dir"
}

# sign <dir> <secret> <passphrase> <public key>: the script as the
# workflow runs it, its temporary directory under $scratch/tmp; prints
# stdout and stderr together.
mkdir -p "$scratch/tmp"
sign() {
  GPG_PRIVATE_KEY="$2" GPG_PASSPHRASE="$3" RUNNER_TEMP="$scratch/tmp" \
    "$script" "$1" "$4" 2>&1
}

# The good run.
dir="$(assets good)"
if output="$(sign "$dir" "$release_secret" "$passphrase" "$scratch/release.asc")"; then
  [[ "$(tail -n 1 <<< "$output")" == "$release" ]] || fail "the last line is not the fingerprint: $output"
else
  fail "a good run failed: $output"
fi
[[ -z "$(ls -A "$scratch/tmp")" ]] || fail "the GNUPGHOME was left behind: $(ls -A "$scratch/tmp")"
[[ "$output" != *"$passphrase"* ]] || fail "the passphrase was printed"
[[ "$output" != *"PRIVATE KEY BLOCK"* ]] || fail "the secret key was printed"

# SHA256SUMS lists every asset once, sorted, and checks.
listed="$(cd "$dir" && awk '{ print $2 }' SHA256SUMS | paste -sd, -)"
wanted='Steno_0.2.0_aarch64.app.tar.gz,Steno_0.2.0_aarch64.app.tar.gz.sig,Steno_0.2.0_aarch64.dmg,Steno_0.2.0_x64-setup.exe,Steno_0.2.0_x64_en-US.msi,latest.json,steno-desktop_0.2.0_amd64.AppImage,steno-desktop_0.2.0_amd64.AppImage.sig,steno-desktop_0.2.0_amd64.deb,steno-desktop_0.2.0_amd64.deb.sig'
[[ "$listed" == "$wanted" ]] || fail "SHA256SUMS lists $listed"
(cd "$dir" && sha256sum --check --quiet --strict SHA256SUMS) || fail "SHA256SUMS does not check"

# Signatures for SHA256SUMS and the two Linux bundles, nothing else, each
# good from the release key as a user verifies it.
signatures="$(cd "$dir" && find . -maxdepth 1 -name '*.asc' | sed 's|^\./||' | LC_ALL=C sort | paste -sd, -)"
[[ "$signatures" == 'SHA256SUMS.asc,steno-desktop_0.2.0_amd64.AppImage.asc,steno-desktop_0.2.0_amd64.deb.asc' ]] \
  || fail "the signatures are $signatures"
user="$scratch/user"
(umask 077 && mkdir "$user")
GNUPGHOME="$user" gpg --batch --quiet --import "$scratch/release.asc"
for file in SHA256SUMS steno-desktop_0.2.0_amd64.AppImage steno-desktop_0.2.0_amd64.deb; do
  status="$(GNUPGHOME="$user" gpg --batch --status-fd 1 --verify "$dir/$file.asc" "$dir/$file" 2>/dev/null || true)"
  grep -q "^\[GNUPG:\] VALIDSIG $release " <<< "$status" || fail "$file.asc does not verify"
done
grep -q 'BEGIN PGP SIGNATURE' "$dir/SHA256SUMS.asc" || fail "SHA256SUMS.asc is not armored"

# A tampered bundle fails its signature and its checksum.
printf 'changed\n' >> "$dir/steno-desktop_0.2.0_amd64.deb"
GNUPGHOME="$user" gpg --batch --verify "$dir/steno-desktop_0.2.0_amd64.deb.asc" \
  "$dir/steno-desktop_0.2.0_amd64.deb" 2>/dev/null && fail "a tampered .deb verified"
(cd "$dir" && sha256sum --check --quiet --strict SHA256SUMS >/dev/null 2>&1) && fail "a tampered .deb checked"
GNUPGHOME="$user" gpgconf --kill all 2>/dev/null || true

# A second run replaces the first's files instead of listing them, and
# prints only the fingerprint on stdout.
dir="$(assets rerun)"
sign "$dir" "$release_secret" "$passphrase" "$scratch/release.asc" >/dev/null || fail "the first of two runs failed"
stdout="$(GPG_PRIVATE_KEY="$release_secret" GPG_PASSPHRASE="$passphrase" RUNNER_TEMP="$scratch/tmp" \
  "$script" "$dir" "$scratch/release.asc" 2>/dev/null)" || fail "the second of two runs failed"
[[ "$stdout" == "$release" ]] || fail "stdout is not the fingerprint alone: $stdout"
grep -qE ' (SHA256SUMS|.*\.asc)$' "$dir/SHA256SUMS" && fail "a rerun listed its own files"

# refuse <why> <secret> <passphrase> <public key> [text]: exit non-zero with
# an ::error:: (holding the text, if given), leaving no GNUPGHOME and no
# secret in the output.
refuse() {
  local why="$1" dir output
  dir="$(assets "refuse-$(tr -c '[:lower:]' - <<< "$why")")"
  if output="$(sign "$dir" "$2" "$3" "$4")"; then
    fail "$why was accepted: $output"
  elif [[ "$output" != *"::error::${5:-}"* ]]; then
    fail "$why did not fail with an ::error::${5:-}: $output"
  fi
  [[ -z "$(ls -A "$scratch/tmp")" ]] || fail "$why left a GNUPGHOME behind"
  [[ "$output" != *"PRIVATE KEY BLOCK"* && "$output" != *"$passphrase"* ]] || fail "$why printed a secret"
}

refuse 'a secret key that is not the public key' "$impostor_secret" other "$scratch/release.asc"
refuse 'a public key that is not the secret key' "$release_secret" "$passphrase" "$scratch/impostor.asc"
refuse 'a wrong passphrase' "$release_secret" 'not it' "$scratch/release.asc"
refuse 'a secret that is no key' 'not a key' "$passphrase" "$scratch/release.asc"
printf 'not a key\n' > "$scratch/garbage.asc"
refuse 'a public key file that is no key' "$release_secret" "$passphrase" "$scratch/garbage.asc"
refuse 'a missing public key' "$release_secret" "$passphrase" "$scratch/missing.asc"
refuse 'an expired key' "$expired_secret" "$passphrase" "$scratch/expired.asc" \
  "the release signing key $expired expired on "
# The check after signing on its own: a gpgv that reports nothing.
stub="$scratch/stub"
mkdir -p "$stub"
printf '#!/bin/sh\nexit 0\n' > "$stub/gpgv"
chmod +x "$stub/gpgv"
PATH="$stub:$PATH" refuse 'a signature gpgv does not report valid' "$release_secret" "$passphrase" \
  "$scratch/release.asc" 'SHA256SUMS.asc does not verify'
empty="$scratch/empty"
mkdir -p "$empty"
if output="$(sign "$empty" "$release_secret" "$passphrase" "$scratch/release.asc")"; then
  fail "an empty directory was accepted: $output"
fi
GPG_PASSPHRASE=x "$script" "$dir" "$scratch/release.asc" >/dev/null 2>&1 && fail "no GPG_PRIVATE_KEY was accepted"

if ((failures > 0)); then
  echo "release-signatures: $failures failed"
  exit 1
fi
echo "release-signatures: ok"
