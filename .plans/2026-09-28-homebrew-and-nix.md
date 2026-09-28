# Steno: Homebrew and Nix installs

Extends [`2026-09-25-macos-app-and-release.md`](2026-09-25-macos-app-and-release.md), whose
non-goals listed "Homebrew cask". The DMG, Sparkle feed and release workflow stay as that plan
describes; this one adds two more ways to install the same notarised DMG. Scope in
[`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md) is unchanged: no App Store, no
hosted service, audio never leaves the device.

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
