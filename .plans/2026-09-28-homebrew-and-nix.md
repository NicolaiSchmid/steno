# Homebrew cask and Nix flake

Adds two install paths for the released macOS app on top of the DMG from
[`2026-09-25-macos-app-and-release.md`](2026-09-25-macos-app-and-release.md),
which listed a Homebrew cask as out of scope for v1. Neither path builds
anything: both unpack the signed, notarised `Steno-<version>.dmg` that
`release.yml` publishes and copy `Steno.app` unchanged. Both are wrappers
around the release, so the release plan still owns signing, notarisation and
Sparkle.

The Homebrew section is written with the tap (`nicolaischmid/tap`, cask
`steno`) and lands in its own PR.

## Nix

### Decision

`flake.nix` at the repository root, `nixpkgs` as the only input. Outputs, all
for `aarch64-darwin` only because the app is Apple Silicon only:

| Output | What it is |
|---|---|
| `packages.aarch64-darwin.steno` (and `default`) | `stdenvNoCC.mkDerivation` over `fetchurl` of the release DMG, unpacked with `undmg`, `Steno.app` copied to `$out/Applications/` |
| `checks.aarch64-darwin.steno-bundle` | asserts `Contents/Info.plist` exists, `CFBundleIdentifier` is `uno.schmid.steno.mac` and `CFBundleShortVersionString` matches the pinned version |
| `formatter` | `alejandra` (the convention in `agent-infra`), also for `x86_64-linux` and `aarch64-linux` so the flake can be formatted from atlas |

Version and SRI hash live in one `release` attrset at the top of the file, so
a bump is two lines. `release.yml` prints exactly those two lines in the job
summary of every tag run; the bump stays a human commit for now.

### What the derivation must not do

The bundle is Developer ID signed and notarised. Every stdenv phase that
would touch it is disabled (`dontFixup`, `dontPatchShebangs`, `dontBuild`,
`dontConfigure`): stripping a Mach-O, patching a shebang inside
`Sparkle.framework`, or re-signing would break the signature Gatekeeper checks
on first launch. Nothing is removed from the bundle for the same reason,
Sparkle included. `codesign --verify --deep --strict` and `spctl --assess` on
the store path are the acceptance test: they pass only if the copy is
byte-identical to what CI notarised.

### Sparkle in the Nix store

The app runs from a read-only store path. Sparkle's "Check for Updates…" will
find a newer release and download it, then fail to install because it cannot
replace the bundle. Two mitigations, neither of which edits the bundle:

- Documented: the package writes `share/doc/steno/UPDATES.md` and the README
  tells Nix users to update by bumping the flake, and how to silence the
  scheduled daily check with
  `defaults write uno.schmid.steno.mac SUEnableAutomaticChecks -bool NO`.
  Sparkle reads `SUEnableAutomaticChecks` from user defaults before
  Info.plist, so this works without touching the bundle.
- App side, post-v1: hide "Check for Updates…" and the Updates settings when
  the bundle is not writable (Nix store, read-only volume). Tracked in the
  `post-v1` `workstream:macos` issue linked from the PR.

nix-darwin users who want the in-app updater should use the Homebrew cask via
`homebrew.casks`; that path installs into `/Applications` where Sparkle can
write.

### Installing

```sh
nix profile install github:NicolaiSchmid/steno#steno
```

nix-darwin: add `steno.packages.aarch64-darwin.steno` (with the flake as an
input) to `environment.systemPackages`; nix-darwin links
`$out/Applications/*.app` into `/Applications/Nix Apps`. home-manager:
`home.packages` plus either `home.file` linking the app into `~/Applications`
or the `mac-app-util` module, because home-manager does not link
`Applications/` on its own. `nix run` does not apply: there is no `bin/`, the
output is an app bundle.

### Verification

Done on Forge (`aarch64-darwin`, Nix 2.34 with flakes) before merging:
`nix build github:NicolaiSchmid/steno/<branch>#steno`, then `codesign
--verify --deep --strict` and `spctl --assess --type execute` on the store
path, and `nix flake check`. Repeated by hand after each `release` bump until
the bump is automated.

### CI

No Nix job in `repository-ci.yml` for now: the runner is `ubuntu-latest`
without Nix, and a Linux `nix flake check` would only evaluate the darwin
outputs with `--all-systems --no-build`, which needs a Nix installer action
first. `release.yml` gains the job-summary step described above; that is the
only workflow change.

### Files

```
flake.nix                                   package, check, formatter
flake.lock                                  pins nixpkgs
README.md                                   Install section, Nix subsection
.github/workflows/release.yml               job summary with version and SRI hash on tag runs
.plans/2026-09-28-homebrew-and-nix.md        this file
```
