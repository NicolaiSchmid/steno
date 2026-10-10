#!/usr/bin/env bash
# Builds, installs and checks the AUR package in packaging/aur/ on Arch
# Linux. Runs as root in a throwaway archlinux:base-devel container (the
# AUR package job in .github/workflows/aur-ci.yml; locally, the docker
# line in packaging/aur/README.md). It builds as an unprivileged user, as
# makepkg requires, and installs packages and adds a user, so run it only in
# a throwaway container. It fails when:
#
# - .SRCINFO differs from `makepkg --printsrcinfo`;
# - keys/pgp/ holds another key than apps/desktop/release-signing-key.asc,
#   validpgpkeys names another fingerprint, or source= lacks the .deb's .asc;
# - LICENSE differs from the repository's, speexdsp-COPYING from
#   crates/steno-audio/vendor/speexdsp/COPYING, or a copy here of a drop-in
#   from its file in apps/desktop/src-tauri/linux/;
# - the PKGBUILD's _copy, while it has one, installs a copy over a file the
#   .deb ships or not where the .deb lacks it;
# - the .deb's checksum or its signature from the release key does not
#   verify, makepkg skipped either check, or the package does not build
#   and install (which includes a copy still here when the .deb ships its
#   file);
# - namcap reports an error;
# - a file is missing: the binary and the sidecar side by side in
#   /usr/lib/steno-desktop, the /usr/bin wrapper, the drop-ins, the
#   desktop entry, the licenses;
# - the wrapper is not executable, or its commands are not exactly the three
#   that set STENO_DISTRIBUTION=aur and STENO_EXEC_PATH=/usr/bin/steno-desktop
#   and run the binary;
# - an installed drop-in is not the copy here while one exists, or its
#   settings are not exactly its section and TimeoutStopSec=20s;
# - the ufw profile does not open the handover's Linux port
#   (HandoverConfiguration::LINUX_PORT in crates/steno-handover), is not
#   installed as /etc/ufw/applications.d/steno-desktop and kept on upgrade
#   (backup=), or `ufw app info Steno` does not read it; or ufw's own rules
#   stop admitting mDNS, which the profile leaves to them;
# - a binary needs a library that is not installed, or the sidecar does
#   not start.
#
#   packaging/check-aur.sh
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
die() { echo "check-aur: $*" >&2; exit 1; }

pacman -Syu --noconfirm --needed namcap >/dev/null

id builder >/dev/null 2>&1 || useradd -m builder
echo 'builder ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/builder
work=/home/builder/aur
rm -rf "$work"
cp -r "$root/packaging/aur" "$work"
chown -R builder: "$work"
as_builder() { su builder -c "cd $work && $1"; }

as_builder 'makepkg --printsrcinfo' > /tmp/srcinfo
diff -u "$work/.SRCINFO" /tmp/srcinfo \
  || die ".SRCINFO is stale: run makepkg --printsrcinfo > .SRCINFO in packaging/aur"
echo "ok: .SRCINFO matches the PKGBUILD"

release_key="$root/apps/desktop/release-signing-key.asc"
fpr="$(gpg --show-keys --with-colons "$release_key" | awk -F: '/^fpr/ { print $10; exit }')"
cmp -s "$release_key" "$work/keys/pgp/$fpr.asc" \
  || die "packaging/aur/keys/pgp/$fpr.asc is not apps/desktop/release-signing-key.asc"
[[ "$(sed -n 's/^\tvalidpgpkeys = //p' "$work/.SRCINFO")" == "$fpr" ]] \
  || die "validpgpkeys is not exactly the release key's fingerprint $fpr"
deb="$(sed -n 's/^\tsource = \(.*_amd64\.deb\)$/\1/p' "$work/.SRCINFO")"
grep -qxF $'\tsource = '"$deb.asc" "$work/.SRCINFO" \
  || die "source= lacks the .deb's signature, its .asc"
echo "ok: the PKGBUILD checks the .deb's .asc against the release key $fpr"

cmp -s "$root/LICENSE" "$work/LICENSE" || die "packaging/aur/LICENSE is not the repository's LICENSE"
cmp -s "$root/crates/steno-audio/vendor/speexdsp/COPYING" "$work/speexdsp-COPYING" \
  || die "packaging/aur/speexdsp-COPYING is not crates/steno-audio/vendor/speexdsp/COPYING"
echo "ok: LICENSE is the repository's, speexdsp-COPYING the vendored SpeexDSP's"

# The stop timeout drop-ins (P5): the path under /usr/lib/systemd/user, the
# file in apps/desktop/src-tauri/linux (and its copy here while the pinned
# .deb lacks it) and the section it sets.
linux="$root/apps/desktop/src-tauri/linux"
drop_ins=(
  'app-steno\x2ddesktop@autostart.service.d/10-steno.conf autostart-service-stop-timeout.conf Service'
  'app-gnome-steno\x2ddesktop-.scope.d/zz-steno.conf gnome-scope-stop-timeout.conf Scope'
)
# A copy that Bump step 3 has deleted is not checked.
for entry in "${drop_ins[@]}"; do
  read -r _ conf _ <<<"$entry"
  [[ ! -e "$work/$conf" ]] || cmp -s "$linux/$conf" "$work/$conf" \
    || die "packaging/aur/$conf is not apps/desktop/src-tauri/linux/$conf"
done
echo "ok: the copies of the drop-ins are apps/desktop/src-tauri/linux's"

# The ufw profile opens the port the app binds on Linux.
port="$(sed -n 's/^ *pub const LINUX_PORT: u16 = \([0-9]*\);$/\1/p' \
  "$root/crates/steno-handover/src/configuration.rs")"
[[ -n "$port" ]] || die "crates/steno-handover/src/configuration.rs has no LINUX_PORT"
[[ "$(grep -E '^ports=' "$work/ufw-steno-desktop")" == "ports=$port/tcp" ]] \
  || die "packaging/aur/ufw-steno-desktop does not open exactly $port/tcp, the handover's LINUX_PORT"
echo "ok: the ufw profile opens $port/tcp, the handover's LINUX_PORT"

# The pinned .deb lacks the drop-ins, so the build never trips _copy's
# guard. While the PKGBUILD has _copy, call it on a stand-in for the
# unpacked .deb: a file there stops it, a missing one is installed.
if grep -q '^_copy()' "$work/PKGBUILD"; then
  fake="$(mktemp -d)"
  # error and pkgdir are makepkg's, for _copy.
  # shellcheck source=/dev/null disable=SC2034,SC2329
  (cd "$work" && source ./PKGBUILD && error() { :; } && pkgdir="$fake" \
    && mkdir "$fake/deb" && touch "$fake/deb/f" && ! _copy LICENSE "$fake/deb/f" \
    && _copy LICENSE "$fake/new/f" && cmp -s LICENSE "$fake/new/f") \
    || die "_copy installs a copy over a file the .deb ships, or not where it lacks one"
  rm -rf "$fake"
  echo "ok: _copy fails on a file the .deb ships and installs one it lacks"
fi

as_builder "gpg --batch --import keys/pgp/$fpr.asc"
as_builder 'makepkg -si --noconfirm' 2>&1 | tee /tmp/makepkg.log
# makepkg prints each header only when it runs that check, and the .deb's
# result (the first source) on the line after it.
passed() {
  awk -v h="^==> $1" '$0 ~ h { getline; print; exit }' /tmp/makepkg.log \
    | grep -qE '_amd64\.deb \.\.\. Passed'
}
passed 'Validating source files with sha256sums' || die "makepkg did not check the .deb's checksum"
passed 'Verifying source file signatures with gpg' || die "makepkg did not verify the .deb's signature"
echo "ok: built, checked against its checksum and the release key, and installed"

namcap "$work/PKGBUILD" "$work"/*.pkg.tar.zst | tee /tmp/namcap
! grep -q ' E: ' /tmp/namcap || die "namcap reports an error"
echo "ok: namcap reports no error"

pkgname=steno-desktop-bin
files="$(pacman -Qlq "$pkgname")"
for path in \
  /usr/bin/steno-desktop \
  /usr/lib/steno-desktop/steno-desktop \
  /usr/lib/steno-desktop/steno-speech-sidecar \
  /usr/share/applications/steno-desktop.desktop \
  /usr/share/licenses/$pkgname/LICENSE \
  /usr/share/licenses/$pkgname/speexdsp-COPYING; do
  grep -qxF "$path" <<<"$files" || die "the package does not install $path"
done
[[ -x /usr/lib/steno-desktop/steno-desktop && -x /usr/lib/steno-desktop/steno-speech-sidecar ]] \
  || die "the binary or the sidecar is not executable"
[[ ! -e /usr/bin/steno-speech-sidecar ]] || die "the sidecar is still in /usr/bin"
echo "ok: the binary and the sidecar sit side by side in /usr/lib/steno-desktop"

[[ -x /usr/bin/steno-desktop ]] || die "the wrapper is not executable"
[[ "$(head -n 1 /usr/bin/steno-desktop)" == '#!/bin/sh' ]] || die "the wrapper is not a sh script"
# Every line but comments and blank ones, so nothing overrides the
# variables or runs first.
grep -vE '^[[:space:]]*(#|$)' /usr/bin/steno-desktop \
  | diff -u <(printf '%s\n' 'export STENO_DISTRIBUTION=aur' \
    'export STENO_EXEC_PATH=/usr/bin/steno-desktop' \
    'exec /usr/lib/steno-desktop/steno-desktop "$@"') - \
  || die "the wrapper's commands differ from the three expected"
echo "ok: the wrapper is an executable sh script that sets STENO_DISTRIBUTION and STENO_EXEC_PATH, then runs the binary"

for entry in "${drop_ins[@]}"; do
  read -r unit conf section <<<"$entry"
  installed="/usr/lib/systemd/user/$unit"
  grep -qxF "$installed" <<<"$files" || die "the package does not install $installed"
  [[ ! -e "$work/$conf" ]] || cmp -s "$installed" "$work/$conf" \
    || die "$installed is not packaging/aur/$conf"
  grep -vE '^[[:space:]]*(#|$)' "$installed" \
    | diff -u <(printf '%s\n' "[$section]" 'TimeoutStopSec=20s') - \
    || die "$installed does not set exactly [$section] TimeoutStopSec=20s"
  echo "ok: the package installs $installed, which sets exactly [$section] TimeoutStopSec=20s"
done

profile=/etc/ufw/applications.d/steno-desktop
grep -qxF "$profile" <<<"$files" || die "the package does not install $profile"
cmp -s "$profile" "$work/ufw-steno-desktop" || die "$profile is not packaging/aur/ufw-steno-desktop"
pacman -Qii "$pkgname" | grep -qE "^(Backup Files[[:space:]]*:[[:space:]]*)?$profile[[:space:]]" \
  || die "$profile is not a backup file, so an upgrade would overwrite a user's edit"
pacman -S --noconfirm --needed ufw >/dev/null
ufw app info Steno | tee /tmp/ufw-app
grep -qxF 'Profile: Steno' /tmp/ufw-app && grep -qxF "  $port/tcp" /tmp/ufw-app \
  || die "ufw app info Steno does not read the profile with $port/tcp"
# mDNS to 224.0.0.251:5353, which ufw's before.rules accept for every
# profile and policy.
grep -qE -- '-d 224\.0\.0\.251 --dport 5353 -j ACCEPT' /etc/ufw/before.rules \
  || die "ufw's before.rules no longer admit mDNS: the profile must open 5353/udp"
echo "ok: the package installs the ufw profile Steno as a backup file, ufw reads it, and ufw admits mDNS"

for binary in /usr/lib/steno-desktop/steno-desktop /usr/lib/steno-desktop/steno-speech-sidecar; do
  ! ldd "$binary" | grep 'not found' || die "$binary needs a library that is not installed"
done
echo "ok: every library both binaries link is installed"

# The sidecar refuses an unknown argument with exit 2 once it runs.
status=0
/usr/lib/steno-desktop/steno-speech-sidecar --check-aur 2>/dev/null || status=$?
[[ "$status" == 2 ]] || die "the sidecar did not start (exit $status)"
echo "ok: the sidecar starts"
