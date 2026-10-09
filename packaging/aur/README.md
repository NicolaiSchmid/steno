# AUR package `steno-desktop-bin`

The Arch Linux package for the desktop app, as published on the AUR (Omarchy
is its main target, stable plan X6 in
[`.plans/2026-10-07-stable-promotion.md`](../../.plans/2026-10-07-stable-promotion.md)).
It repackages the release `.deb`; nothing is built from source.

This README is for the maintainer: installing a candidate, bumping the
version and pushing to the AUR. Once the package is on the AUR (from
`v0.11.0`) and the release key is on keyserver.ubuntu.com (**Nicolai**),
users install with `yay -S steno-desktop-bin`; until then, import the key as
below first.

## What the package holds

| Path | From |
|---|---|
| `/usr/lib/steno-desktop/steno-desktop`, `/usr/lib/steno-desktop/steno-speech-sidecar` | The `.deb`'s `/usr/bin`. They move together because the app starts the sidecar from beside its own binary |
| `/usr/bin/steno-desktop` | [`steno-desktop.sh`](steno-desktop.sh): sets `STENO_DISTRIBUTION=aur` (the in-app updater stays off; pacman updates) and `STENO_EXEC_PATH=/usr/bin/steno-desktop` (the path the autostart entry names), then runs the binary. Both take effect from the first release that contains #261; rc.3 ignores them |
| `/usr/share/applications/steno-desktop.desktop`, `/usr/share/icons/hicolor/*/apps/steno-desktop.png` | The `.deb`, unchanged. The entry's `Exec=steno-desktop` finds the wrapper on `PATH` |
| `/usr/lib/systemd/user/app-steno\x2ddesktop@autostart.service.d/10-steno.conf`, `/usr/lib/systemd/user/app-gnome-steno\x2ddesktop-.scope.d/zz-steno.conf` | The stop timeout drop-ins (P5): the `.deb`'s from the first release that contains #227; until then the copies here |
| `/usr/share/licenses/steno-desktop-bin/LICENSE` | [`LICENSE`](LICENSE), a copy of the repository's |
| `/usr/share/licenses/steno-desktop-bin/speexdsp-COPYING` | [`speexdsp-COPYING`](speexdsp-COPYING), a copy of `crates/steno-audio/vendor/speexdsp/COPYING`: the binary compiles in SpeexDSP's echo canceller, and its BSD licence asks for the notice with the binary |

There is no install script. Arch's `systemd` package reloads every running
user manager after any package changes a file under `usr/lib/systemd/user/`
(`30-systemd-daemon-reload-user.hook`), so a running Steno picks the drop-ins
up at the install. From the first release that contains #227, the autostart
entry the app keeps until it exits keeps a running, autostarted Steno safe
through that reload (P5); step 8 of the Omarchy gate, which #227 adds, tests
it.

Still to come: X4's `ufw` profile and X2's Hyprland window rules. The
`TODO` lines in `package()` say where each goes.

## The pinned release

`pkgver=0.1.0rc3` pins `desktop-v0.1.0-rc.3`, which contains neither #227
nor #261. It builds and checks this directory; it is not a release to
publish:

- rc.3 ignores `STENO_DISTRIBUTION` and `STENO_EXEC_PATH` (X5, #261): its
  in-app updater stays on, and the autostart entry its first launch
  writes names `/usr/lib/steno-desktop/steno-desktop`, not the wrapper.
- rc.3 removes its autostart entry as soon as launch at login is turned off
  (P5, #227). The reload during any install that touches
  `usr/lib/systemd/user/` (this package's, or `pipewire`'s or `systemd`'s)
  then unloads a running, autostarted Steno's unit, and the end of the
  session stops it without SIGTERM, so it cannot save the recording: P3
  salvages the meeting as `failed`, without its unsaved tail.
- That entry survives the upgrade to a release that contains #261, and
  every login then starts the new binary without the wrapper: the in-app
  updater stays on and the entry keeps the binary's path. Once on such a
  release, quit Steno, start it from the launcher and turn Launch at
  login off and on; the entry then names `/usr/bin/steno-desktop`. In a
  Steno a login started, turning it on fails with "Steno can't open at
  login from where it's installed now", and launch at login stays off
  until it is turned on from the launcher.

The first AUR push is a release that contains both #227 and #261.

## Install a candidate

`makepkg` checks the `.deb` against its `.asc` with the release key, so
import the key once. From this directory:

```sh
gpg --import keys/pgp/048B527950E4F609B90E63495F8810A6E6D4DB46.asc
# or, without a checkout:
curl -fsSL https://raw.githubusercontent.com/NicolaiSchmid/steno/main/packaging/aur/keys/pgp/048B527950E4F609B90E63495F8810A6E6D4DB46.asc | gpg --import
```

`validpgpkeys` in the PKGBUILD accepts only this fingerprint, so a different
key fails the check. `keys/pgp/` is a copy of
`apps/desktop/release-signing-key.asc`; `check-aur.sh` fails when they
differ. The key is on no keyserver yet, so `gpg --recv-keys` and the import
that AUR helpers such as yay and paru run for a missing `validpgpkeys` key
both fail; with the key imported as above, they verify with it.

Then, for a new candidate after Bump steps 1 to 5, build and install:

```sh
makepkg -si
```

`makepkg` fails with "One or more PGP signatures could not be verified" when
the key is missing, and with "One or more files did not pass the validity
check" when a checksum is wrong.

An upgrade replaces the files under a running Steno: it keeps running the
old binary, but starts the new sidecar for the next meeting it processes.
Once no recording runs, quit Steno and start it again from the launcher.

Quit Steno before `pacman -R steno-desktop-bin`: the removal takes the
drop-ins away, so a running, autostarted Steno has the generator's 5 s stop
timeout until it exits. The removal leaves `~/.local/share/Steno` and
`~/.config` as they are.

## Bump to a new release

1. In `PKGBUILD`, set `pkgver` to the release version in pacman's form:
   `0.11.0-rc.1` becomes `0.11.0rc1`, `0.11.0` stays. pacman ranks
   `0.11.0rc1` below `0.11.0` (`vercmp 0.11.0rc1 0.11.0` prints `-1`), so a
   stable release upgrades a candidate. Set `pkgrel=1`. `_tag` follows from
   `pkgver`: `desktop-v<version>` up to `0.1.0-rc.*`, `v<version>` from
   `0.11.0` on (D1).
2. Update the checksums: `updpkgsums` (from `pacman-contrib`) downloads the
   `.deb` and its `.asc` and rewrites `sha256sums`. The `.deb`'s line must
   match its line in the release's `SHA256SUMS`.
3. Once the release's `.deb` installs the drop-ins (the first release that
   contains #227), delete `autostart-service-stop-timeout.conf`,
   `gnome-scope-stop-timeout.conf`, their `source` and `sha256sums` entries
   and the two `install` lines; `package()` then keeps the `.deb`'s files.
4. Regenerate the metadata the AUR reads: `makepkg --printsrcinfo > .SRCINFO`.
5. Install it (`makepkg -si`) and check that `/usr/lib/steno-desktop/` holds
   both binaries and that Steno starts from the launcher.
6. Commit `PKGBUILD` and `.SRCINFO` here, then, for a stable release, push
   to the AUR.

The release key expires on 2029-10-04, and `makepkg` then refuses every new
release. When the key is extended (`apps/desktop/README.md`, "Checksums and
OpenPGP signatures"), copy the new public key over `keys/pgp/` too.

`packaging/check-aur.sh` checks the checksums, the signature, `.SRCINFO`
and the installed files in a throwaway Arch container (the AUR package job
in `.github/workflows/aur-ci.yml` runs it on every pull request that touches
this directory, the drop-ins in `apps/desktop/src-tauri/linux/`, the release
key, `LICENSE` or SpeexDSP's `COPYING`). From the checkout's root:

```sh
docker run --rm --cpus=2 --memory=4g -v "$PWD":/src:ro \
  archlinux:base-devel bash /src/packaging/check-aur.sh
```

## Push to the AUR

The AUR repository holds the files `makepkg` needs, flat at its root:
`PKGBUILD`, `.SRCINFO`, `.gitignore`, every local file in `source=` (today
`steno-desktop.sh`, the two `.conf` files, `LICENSE` and
`speexdsp-COPYING`) and `keys/pgp/`,
the release key. Not this README.

**The first push is by hand** (Nicolai, at gate G3, with a release that
contains #227 and #261), from an AUR account with an SSH key added under
"My Account":

```sh
git clone ssh://aur@aur.archlinux.org/steno-desktop-bin.git /tmp/aur-steno
cd /tmp/aur-steno
cp -r <checkout>/packaging/aur/{PKGBUILD,.SRCINFO,.gitignore,steno-desktop.sh,LICENSE,speexdsp-COPYING,keys} .
cp <checkout>/packaging/aur/*.conf .  # until Bump step 3 deletes them
git add -A
git commit -m "steno-desktop-bin 0.11.0-1"
git push origin master
```

Cloning a name nobody owns yet gives an empty repository, and the first push
creates the package. The AUR accepts only the `master` branch.

**From the second stable release on, the release workflow is to push**
(stable plan S7, "The workflow after S7"; not built yet). Nicolai creates an
SSH key for the AUR account and stores its private half as the repository
secret `AUR_SSH_PRIVATE_KEY`; the `publish` job will then bump and push for
each stable release, and open a pull request with the same bump here, so
this directory does not fall behind the AUR. Candidates are never pushed:
they are installed with `makepkg -si` from this directory after Bump steps
1 to 5.
