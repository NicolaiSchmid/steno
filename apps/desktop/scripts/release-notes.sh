#!/usr/bin/env bash
# The notes of a release (Markdown on stdout): what it carries, that the
# Windows installers are not code-signed, and how to check a download
# against `SHA256SUMS` and the OpenPGP signatures release-signatures.sh
# wrote into <dir>, naming the signing key's fingerprint from <public key>.
# Every 0.11.0 candidate and 0.11.0 itself also say what the new identifier
# means for an install of a desktop build from before it; 0.11.0 also says
# what the Swift app's users get with the handoff (stable plan S7,
# "release-notes.sh"). A release without a hyphen also publishes
# `appcast.xml`, which `SHA256SUMS` does not cover.
#
#   apps/desktop/scripts/release-notes.sh <version> <tag> <dir> apps/desktop/release-signing-key.asc
#
# The public key's link points at the copy in the repository at <tag>, in
# $GITHUB_REPOSITORY (NicolaiSchmid/steno when unset).
# apps/desktop/scripts/release-notes.test.sh checks it; rust-ci.yml runs that.
set -euo pipefail

version="${1:?version, e.g. 0.11.0}"
tag="${2:?the release tag, e.g. v0.11.0}"
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

unsummed="the OpenPGP signatures (\`.asc\`)"
[[ "$version" == *-* ]] || unsummed+=" and \`appcast.xml\`"

cat <<EOF
Steno $version for macOS (Apple silicon, signed and notarised), Linux (\`.deb\`, AppImage) and Windows (\`.msi\`, NSIS \`-setup.exe\`).
EOF
case "$version" in
  0.11.0 | 0.11.0-rc.*)
    cat <<'EOF'

**From an earlier Steno desktop build** (0.1.0): the app's identifier is now `com.nicolaischmid.steno.desktop`, and such an install converts to it when it updates. On a Mac it then asks once more for its permissions (microphone, system audio, calendar) and once for each of its keychain items; choose *Always Allow*. A Mac that ran the Swift app and the desktop app from two paths keeps two copies of one app once both have updated; the one outside `/Applications` can be deleted.
EOF
    ;;
esac
if [[ "$version" == 0.11.0 ]]; then
  cat <<'EOF'

### From the Swift app

- Steno for Mac 0.10 and earlier receives this release as an update, and becomes this app at the same path.
- macOS asks once more for microphone, system audio and calendar access, and for the login keychain password once for each secret Steno stored; choose *Always Allow*.
- An old "Steno" entry in System Settings > General > Login Items may need removing.
- Phones stay paired. If you deny macOS's prompt for the pairing key, Steno keeps the phones waiting and offers *Try again* in Settings > Phones. A phone pairs again only if it was paired with an earlier Steno desktop build on this Mac and never with the Swift app, or when you choose *Pair again*.
- The optional audio mixdown is now WAV.
- Whisper, Parakeet Ultra and the German Parakeet are gone: Steno now transcribes with Parakeet v3.
- Homebrew: if the app updated itself, run `brew upgrade --greedy --cask nicolaischmid/tap/steno` so Homebrew knows.

**Omarchy and Arch:** `yay -S steno-desktop-bin`. **NixOS:** the flake's package and module, `inputs.steno.url = "github:NicolaiSchmid/steno/v0.11.0";` with `steno.nixosModules.default` and `programs.steno.enable = true;` (the desktop README has the details).

**Known issues on Linux:**

- On KDE Plasma a logout does not wait for a recording until Plasma calls the desktop portal's session monitor; Steno saves when the display closes.
- The system audio lane records everything that plays on the default output, not only the call.
- Very long sessions can run into WebKitGTK's file descriptor leak ([#160](https://github.com/NicolaiSchmid/steno/issues/160)).
- GNOME shows no tray icon without the AppIndicator extension; closing the main window then quits Steno, and saves a recording first.
- On Hyprland, start Steno from the app launcher or with `uwsm-app -- steno-desktop`: one started outside `uwsm` runs in Hyprland's own unit, which stops it at logout after at most 10 s.
EOF
fi
cat <<EOF

**Windows:** the installers are not code-signed yet, so SmartScreen warns before the first install ("Windows protected your PC"; choose *More info*, then *Run anyway*). Check the installer against \`SHA256SUMS\` first: in PowerShell, \`(Get-FileHash .\\<installer>).Hash -eq '<its hash in SHA256SUMS>'\` must print \`True\`.

### Verify a download

\`SHA256SUMS\` lists every other file of this release but $unsummed. It and each Linux bundle have a detached OpenPGP signature (\`<file>.asc\`) from the Steno release signing key:

    $grouped

Download \`SHA256SUMS\`, \`SHA256SUMS.asc\` and the files you want into one directory and run these commands there; \`wget\` fetches the key where \`curl\` is missing:

${fence}sh
curl -fsSLO $key_url
gpg --import release-signing-key.asc
gpg --verify SHA256SUMS.asc SHA256SUMS
sha256sum --check --ignore-missing SHA256SUMS
EOF
if [[ ${#bundles[@]} -gt 0 ]]; then printf '%s\n' "${bundles[@]}"; fi
cat <<EOF
$fence

Each \`gpg --verify\` must report "Good signature" from the fingerprint above, and \`sha256sum\` must print OK for every file you downloaded; the warning that the key is not certified by a trusted signature is expected. The \`.sig\` files are the in-app updater's signatures, not OpenPGP ones.
EOF
