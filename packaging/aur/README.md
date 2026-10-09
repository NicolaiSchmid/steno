# AUR package `steno-desktop-bin`

The Arch Linux package for the desktop app, as published on the AUR (Omarchy
is its main target, stable plan X6 in
[`.plans/2026-10-07-stable-promotion.md`](../../.plans/2026-10-07-stable-promotion.md)).
It repackages the release `.deb`; nothing is built from source.

This README is for the maintainer: installing a candidate, bumping the
version and pushing to the AUR. Users install with `yay -S steno-desktop-bin`.

## What the package holds

| Path | From |
|---|---|
| `/usr/lib/steno-desktop/steno-desktop`, `/usr/lib/steno-desktop/steno-speech-sidecar` | The `.deb`'s `/usr/bin`. They move together because the app starts the sidecar from beside its own binary |
| `/usr/bin/steno-desktop` | [`steno-desktop.sh`](steno-desktop.sh): sets `STENO_DISTRIBUTION=aur` (the in-app updater stays off; pacman updates) and `STENO_EXEC_PATH=/usr/bin/steno-desktop` (the path the autostart entry names), then runs the binary |
| `/usr/share/applications/steno-desktop.desktop`, `/usr/share/icons/hicolor/*/apps/steno-desktop.png` | The `.deb`, unchanged. The entry's `Exec=steno-desktop` finds the wrapper on `PATH` |
| `/usr/lib/systemd/user/app-steno\x2ddesktop@autostart.service.d/10-steno.conf`, `/usr/lib/systemd/user/app-gnome-steno\x2ddesktop-.scope.d/zz-steno.conf` | The stop timeout drop-ins (P5): the `.deb`'s from the first release after #227, until then the copies here |
| `/usr/share/licenses/steno-desktop-bin/LICENSE` | [`LICENSE`](LICENSE) |

There is no install script. Arch's `systemd` package reloads every running
user manager after any package changes a file under `usr/lib/systemd/user/`
(`30-systemd-daemon-reload-user.hook`), so a running Steno picks the drop-ins
up at the install. What keeps a running, autostarted Steno safe through that
reload is the autostart entry the app keeps until it exits (P5); step 8 of
the Omarchy gate tests it.

Still to come: X4's `ufw` profile and X2's Hyprland window rules. The
`TODO` lines in `package()` say where each goes.

## Install a candidate

The `.deb` is checked against its `.asc` with the release key, so import the
key once:

```sh
gpg --recv-keys 048B527950E4F609B90E63495F8810A6E6D4DB46
# or, from a checkout:
gpg --import apps/desktop/release-signing-key.asc
```

Then, from this directory:

```sh
makepkg -si
```

`makepkg` fails with "One or more PGP signatures could not be verified" when
the key is missing, and with "One or more files did not pass the validity
check" when a checksum is wrong.

## Bump to a new release

1. In `PKGBUILD`, set `pkgver` to the release version in pacman's form:
   `0.11.0-rc.1` becomes `0.11.0rc1`, `0.11.0` stays. pacman ranks
   `0.11.0rc1` below `0.11.0` (`vercmp 0.11.0rc1 0.11.0` prints `-1`), so a
   stable release upgrades a candidate. Set `pkgrel=1`.
2. Check `_tag`: the releases up to `0.1.0-rc.*` are tagged
   `desktop-v<version>`; from `0.11.0` on they are `v<version>` (D1), so the
   line becomes `_tag=v$_version`.
3. Update the checksums: `updpkgsums` (from `pacman-contrib`) downloads the
   `.deb` and its `.asc` and rewrites `sha256sums`. The `.deb`'s line must
   match its line in the release's `SHA256SUMS`.
4. Once the release's `.deb` installs the drop-ins (the first release after
   #227 merges), delete `autostart-service-stop-timeout.conf`,
   `gnome-scope-stop-timeout.conf`, their `source` and `sha256sums` entries
   and the two `install` lines; `package()` then keeps the `.deb`'s files.
5. Regenerate the metadata the AUR reads: `makepkg --printsrcinfo > .SRCINFO`.
6. Install it (`makepkg -si`) and check that `/usr/lib/steno-desktop/` holds
   both binaries and that Steno starts from the launcher.
7. Commit `PKGBUILD` and `.SRCINFO` here, then push to the AUR.

`packaging/check-aur.sh` checks all of this in a throwaway Arch container
(the AUR package job in `.github/workflows/aur-ci.yml` runs it on every pull
request that touches this directory):

```sh
docker run --rm --cpus=2 --memory=4g -v "$PWD":/src:ro \
  archlinux:base-devel bash /src/packaging/check-aur.sh
```

## Push to the AUR

The AUR repository holds the files `makepkg` needs, flat at its root:
`PKGBUILD`, `.SRCINFO`, `.gitignore` and every local file in `source=`
(today `steno-desktop.sh`, the two `.conf` files and `LICENSE`). Not this
README.

**The first push is by hand** (Nicolai, at gate G3), from an AUR account
with an SSH key added under "My Account":

```sh
git clone ssh://aur@aur.archlinux.org/steno-desktop-bin.git /tmp/aur-steno
cd /tmp/aur-steno
cp <checkout>/packaging/aur/{PKGBUILD,.SRCINFO,.gitignore,steno-desktop.sh,LICENSE,*.conf} .
git add -A
git commit -m "steno-desktop-bin 0.11.0-1"
git push origin master
```

Cloning a name nobody owns yet gives an empty repository, and the first push
creates the package. The AUR accepts only the `master` branch.

**From the second stable release on, the release workflow pushes.** Nicolai
creates an SSH key for the AUR account and stores its private half as the
repository secret `AUR_SSH_PRIVATE_KEY`; the `publish` job then bumps and
pushes for each stable release (`.github/workflows/desktop-release.yml`, stable plan
"The workflow after S7"). Candidates are never pushed: they are installed
with `makepkg -si` from this directory.
