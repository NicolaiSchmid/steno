# Homebrew cask and Nix flake

Adds two install paths for the released macOS app on top of the DMG from
[`2026-09-25-macos-app-and-release.md`](2026-09-25-macos-app-and-release.md),
which listed a Homebrew cask as out of scope for v1. Neither path builds
anything: both unpack the signed, notarised `Steno-<version>.dmg` that
`release.yml` publishes and copy `Steno.app` unchanged. Both are wrappers
around the release, so the release plan still owns signing, notarisation and
Sparkle.

## Homebrew

### Decisions

- **Own tap, not homebrew-cask.** [NicolaiSchmid/homebrew-tap](https://github.com/NicolaiSchmid/homebrew-tap),
  public, `Casks/steno.rb`. homebrew-cask wants a notable, stable project and rejects
  pre-releases; the tap can carry release candidates and is ours to bump.
- **Install line**: `brew tap nicolaischmid/tap && brew install --cask steno`. nix-darwin:
  `homebrew.taps = [ "nicolaischmid/tap" ]; homebrew.casks = [ "nicolaischmid/tap/steno" ];`.
- **Cask stanzas**: `version`, `sha256` of the DMG, `url` derived from the version
  (`releases/download/v#{version}/Steno-#{version}.dmg`, the name `build-release.sh` and
  `make-dmg.sh` produce), `auto_updates true` (Sparkle owns updates, `brew upgrade` does not
  reinstall an app that updated itself), `depends_on macos: ">= :sequoia"` and
  `depends_on arch: :arm64` (matching `MACOSX_DEPLOYMENT_TARGET` and the build), `app
  "Steno.app"`, `zap trash` for `~/Library/Application Support/Steno`, the preferences plist,
  Caches, HTTPStorages and Saved Application State of `uno.schmid.steno.mac`, and a
  `caveats` line that says Sparkle updates the app and `brew upgrade` follows releases too.
  Nothing writes to `~/Library/Logs/Steno`, so it is not zapped.
- **Livecheck** uses `strategy :github_releases` with a block that keeps pre-releases (only
  drafts are dropped) and a regex accepting `v1.2.3` and `v1.2.3-rc.1`. `:github_latest` would
  follow `releases/latest`, which is exactly what Sparkle does and exactly what the tap must
  not do while it tracks release candidates; with only pre-releases published, `latest` is a
  404 and `brew livecheck` would error. Livecheck is informational; the bump is automated.
- **Pre-releases bump the cask.** The release workflow bumps on every `v*` tag, hyphenated or
  not. Rationale: release candidates are meant to be tested through brew, and stable users are
  unaffected because Sparkle reads `releases/latest/download/appcast.xml`, which skips
  pre-releases. Consequence to know: someone who installed an rc through brew gets the next
  stable when Sparkle offers it or when they run `brew upgrade --cask steno`; between the rc
  and the stable tag, `brew install` hands out the rc. Acceptable for a tap whose audience is
  Nicolai and friends; revisit (a `steno@stable` cask or a stable-only tap) if that changes.
- **Bump mechanism**: `apps/macos/scripts/bump-homebrew-cask.sh <version> <dmg>`, called by
  `release.yml` right after "Publish GitHub release" on tags only. It hashes the DMG the
  workflow just uploaded (so the sha in the cask is the sha of the bytes on the release, not
  a second download), clones the tap with `HOMEBREW_TAP_TOKEN`, rewrites the `version` and
  `sha256` lines with `sed`, commits `steno <version>` as `github-actions[bot]` and pushes to
  `main`. Without the token it prints a `::notice::` and exits 0; with it, the step is
  `continue-on-error`, so a failed push is a warning on a release that is already published,
  never a failed release. `check-release-secrets.sh` does not list the token: it is optional.
- **Token**: fine-grained PAT, repository access `homebrew-tap` only, permission Contents
  read and write, stored as the repository secret `HOMEBREW_TAP_TOKEN` on `NicolaiSchmid/steno`
  (`gh secret set HOMEBREW_TAP_TOKEN -R NicolaiSchmid/steno`). `github.token` cannot reach
  another repository, hence the PAT.
- **First cask** is `0.9.0-rc.1`, committed by hand from the notarised DMG on the pre-release
  (`sha256 cb70e39c…7b7ddf`). Every later version comes from the workflow.

### Validation

`brew style --cask` and `brew audit --cask --online` need Homebrew, which neither the Linux
agent host nor Forge has; the cask was checked with `ruby -c` and a stub evaluation of the DSL,
the livecheck regex against sample tags, and the URL against the published asset. Run
`brew audit --cask nicolaischmid/tap/steno` once on a Mac with Homebrew; if it complains, fix
the tap directly, it is not gated on Steno CI. `ReleaseScriptsTests` runs the bump script
against a local bare repository (version and sha rewritten, commit subject `steno <version>`,
notice and exit 0 without a token) and parses it under `/bin/bash`.

### Files

```
apps/macos/scripts/bump-homebrew-cask.sh     rewrite version and sha256 in the tap, push
.github/workflows/release.yml                "Bump Homebrew cask" step after the release is published
apps/macos/StenoTests/ReleaseScriptsTests.swift  bump script against a bare repo; no token means notice
apps/macos/README.md                          step 7 of "Cutting a release", the optional secret, Homebrew section
README.md                                     Install section
```

Tap: `NicolaiSchmid/homebrew-tap` with `README.md` and `Casks/steno.rb`.

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
first. `release.yml` gains the job-summary step described above, right after
the Homebrew bump; those two steps are the only workflow changes.

### Files

```
flake.nix                                   package, check, formatter
flake.lock                                  pins nixpkgs
README.md                                   Install section, Nix subsection
.github/workflows/release.yml               job summary with version and SRI hash on tag runs
.plans/2026-09-28-homebrew-and-nix.md        this file
```
