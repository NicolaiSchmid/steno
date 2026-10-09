#!/usr/bin/env bash
# Builds, installs and checks the AUR package in packaging/aur/ on Arch
# Linux. Runs as root in a throwaway archlinux:base-devel container (the
# AUR package job in .github/workflows/aur-ci.yml; locally, the docker
# line in packaging/aur/README.md), builds as an unprivileged user, as
# makepkg requires, and changes the container's system. It fails when:
#
# - .SRCINFO differs from `makepkg --printsrcinfo`;
# - the .deb's checksum or its signature from the release key does not
#   verify, or the package does not build and install;
# - namcap reports an error;
# - a file is missing: the binary and the sidecar side by side in
#   /usr/lib/steno-desktop, the /usr/bin wrapper, the drop-ins, the
#   desktop entry, the license;
# - the wrapper does not set STENO_DISTRIBUTION=aur and
#   STENO_EXEC_PATH=/usr/bin/steno-desktop, or runs another binary;
# - an installed drop-in differs from apps/desktop/src-tauri/linux/
#   (once that directory holds it);
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

su builder -c 'gpg --batch --import' < "$root/apps/desktop/release-signing-key.asc"

as_builder 'makepkg --printsrcinfo' > /tmp/srcinfo
diff -u "$work/.SRCINFO" /tmp/srcinfo \
  || die ".SRCINFO is stale: run makepkg --printsrcinfo > .SRCINFO in packaging/aur"
echo "ok: .SRCINFO matches the PKGBUILD"

as_builder 'makepkg -si --noconfirm'
echo "ok: built, verified against the release key and installed"

package="$(ls "$work"/*.pkg.tar.zst)"
namcap "$work/PKGBUILD" "$package" | tee /tmp/namcap
! grep -q ' E: ' /tmp/namcap || die "namcap reports an error"
echo "ok: namcap reports no error"

pkgname=steno-desktop-bin
files="$(pacman -Qlq "$pkgname")"
for path in \
  /usr/bin/steno-desktop \
  /usr/lib/steno-desktop/steno-desktop \
  /usr/lib/steno-desktop/steno-speech-sidecar \
  '/usr/lib/systemd/user/app-steno\x2ddesktop@autostart.service.d/10-steno.conf' \
  '/usr/lib/systemd/user/app-gnome-steno\x2ddesktop-.scope.d/zz-steno.conf' \
  /usr/share/applications/steno-desktop.desktop \
  /usr/share/licenses/$pkgname/LICENSE; do
  grep -qxF "$path" <<<"$files" || die "the package does not install $path"
done
[[ -x /usr/lib/steno-desktop/steno-desktop && -x /usr/lib/steno-desktop/steno-speech-sidecar ]] \
  || die "the binary or the sidecar is not executable"
[[ ! -e /usr/bin/steno-speech-sidecar ]] || die "the sidecar is still in /usr/bin"
echo "ok: the binary and the sidecar sit side by side in /usr/lib/steno-desktop"

wrapper=/usr/bin/steno-desktop
for line in 'export STENO_DISTRIBUTION=aur' \
  'export STENO_EXEC_PATH=/usr/bin/steno-desktop' \
  'exec /usr/lib/steno-desktop/steno-desktop "$@"'; do
  grep -qxF "$line" "$wrapper" || die "the wrapper lacks the line: $line"
done
echo "ok: the wrapper sets STENO_DISTRIBUTION and STENO_EXEC_PATH"

linux="$root/apps/desktop/src-tauri/linux"
drop_in() {
  local installed="/usr/lib/systemd/user/$1"
  if [[ -e "$linux/$2" ]]; then
    cmp -s "$installed" "$linux/$2" || die "$installed is not linux/$2"
    echo "ok: $installed is linux/$2"
  fi
}
drop_in 'app-steno\x2ddesktop@autostart.service.d/10-steno.conf' autostart-service-stop-timeout.conf
drop_in 'app-gnome-steno\x2ddesktop-.scope.d/zz-steno.conf' gnome-scope-stop-timeout.conf

for binary in /usr/lib/steno-desktop/steno-desktop /usr/lib/steno-desktop/steno-speech-sidecar; do
  ! ldd "$binary" | grep 'not found' || die "$binary needs a library that is not installed"
done
echo "ok: every library both binaries link is installed"

# The sidecar refuses an unknown argument with exit 2 once it runs.
status=0
/usr/lib/steno-desktop/steno-speech-sidecar --check-aur 2>/dev/null || status=$?
[[ "$status" == 2 ]] || die "the sidecar did not start (exit $status)"
echo "ok: the sidecar starts"
