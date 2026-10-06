# Stable promotion: the Tauri app becomes the Steno release

Status: planned 2026-10-07, not started; waits for Nicolai to confirm D1 to D10.
Nicolai decided on 2026-10-07 to promote the Tauri app from beta to stable:
release tags and the build pipeline produce the Tauri app, the landing page
offers it, and existing users of the Swift Mac app end up on it without losing
meetings, settings, secrets, paired phones or the login item.

This plan extends `.plans/2026-10-04-mac-cutover.md` and supersedes parts of it:

- the cutover plan's opening gate (every unticked parity line) becomes the
  blocking list below (D3);
- its step 2 (the Sparkle handoff) and step 3 (distribution) are replaced by
  "Release mechanics" and "The Sparkle handoff" here;
- its open choices in steps 1 and 5 get proposed answers (D5, D4).

The cutover plan's inventory table, steps 4, 6 and 7, and its tests still apply,
and the rehearsal below runs the tests. Two of its risks and one detail of its
tests no longer hold: the beta-channel staging (Facts), the two-login-items risk
(moot under D4), and test 1's "a Swift release build reads no channel" (every
installed Swift build is a release candidate and reads `beta`). In
`.plans/2026-10-02-rust-core-and-tauri-shell.md`, this plan owns every **WP9b.**
item in "Open after the port".

## Facts this plan rests on

Checked on 2026-10-07 against `main` (`e13ab9b8`), the releases, the `appcast`
branch, the released bundles and the Sparkle 2.10.0 source and tarball.

- **Every release so far is a pre-release.** Swift: `v0.9.0-rc.1` to
  `v0.10.0-rc.2`. Tauri: `desktop-v0.1.0-rc.1`, `desktop-v0.1.0-rc.2` and the
  rolling `desktop-beta` lane. `GET /releases/latest` answers 404. The site's
  Download button (`site.download` in `apps/site/src/lib/site.ts`) and the
  README's "latest release" link land on the releases list, not on a download.
- **Swift installs read one of two feeds.**
  - `v0.9.0-rc.1` (build 244) reads
    `https://github.com/NicolaiSchmid/steno/releases/latest/download/appcast.xml`
    and only Sparkle's default channel. That URL has answered 404 since the
    first release, so these installs have never updated. The Homebrew cask is
    still at `0.9.0-rc.1` (`HOMEBREW_TAP_TOKEN` was never set), so every
    Homebrew install is in this group.
  - `v0.9.0-rc.2` to `v0.10.0-rc.2` read the `appcast` branch
    (`https://raw.githubusercontent.com/NicolaiSchmid/steno/appcast/appcast.xml`).
    They are release candidates, so they read the `beta` channel too
    (`UpdateChannels.allowed` in
    `apps/macos/Steno/Services/UpdateChannels.swift`).
  Every item on the branch carries `beta`, so a beta item would reach every
  install on the branch feed at once. Staging a handoff on the beta channel
  holds no one back; the rehearsal stages it on a local feed instead.
- **Build numbers.** Sparkle orders updates by `CFBundleVersion` alone, and its
  installer refuses a lower one. The newest Swift build is 542
  (`v0.10.0-rc.2`). The Tauri bundle's `CFBundleVersion` is its marketing version
  today ("0.1.0-rc.2"), which Sparkle ranks below 542. `main` has 2068 commits,
  so the commit count, which is the Swift scheme, is far above 542. Tauri 2.10's
  bundler takes `bundle.macOS.bundleVersion`.
- **What Sparkle 2.10 checks.**
  - The archive's EdDSA signature must verify against the running app's
    `SUPublicEDKey`.
  - The new bundle must still carry a public key: Sparkle supports rotating the
    key, not removing it.
  - With the EdDSA check passed, the new bundle's code signature only has to be
    valid.
  - Sparkle finds the new app in the archive by file name (`Steno.app`), and by
    bundle id only as a fallback; it does not compare bundle ids.
  So the bundle id matters for TCC, the keychain and the login item, not for
  Sparkle.
- **Designated requirements.** The Swift app carries Xcode's form of the
  requirement and the Tauri bundler writes `codesign`'s default form, so the two
  texts never match. What TCC and the keychain evaluate is whether the new code
  satisfies the stored requirement, which `codesign --verify -R` checks.
- **Both desktop pre-releases published without manual steps** (runs
  37349614159 and 37425455820): the `publish` job worked, and so did an MSI for
  an `-rc.N` version. The **WP9b.** item about the first `desktop-v*` tag is
  closed, and this plan's PR deletes it.
- **The phone finds the computer by `macID`, not by name.** `macID` is derived
  from the handover certificate's fingerprint (`HandoverIdentity::mac_id`), and
  the phone looks the service up by it (`findByMacID` in
  `mobile/src/features/sync/use-upload-coordinator.ts`). A new identity
  therefore breaks every pairing, not only the pinning.
- **Asset names differ.** The Swift release carries `Steno-<v>.dmg`. The Tauri
  release carries `Steno_<v>_aarch64.dmg`, `Steno_<v>_aarch64.app.tar.gz`,
  `Steno_<v>_x64_en-US.msi`, `Steno_<v>_x64-setup.exe`, and
  `steno-desktop_<v>_amd64.deb` and `.AppImage`. The cask and `flake.nix` build
  the Swift URL, and the flake checks for an executable named `Steno`.
- **The schemas match.** Swift and Rust both stand at migrations 1 to 4, at
  `v0.10.0-rc.2` and at `main`. A Swift app put back over the Tauri app opens
  the database.

## Decisions to confirm

Each decision names the recommended choice, then the alternative. The body of
this plan assumes the recommendation; an alternative changes only the lines
the decision names.

- **D1 Tags are `v*` for the Tauri app.** There is one product, so one tag
  line. The cask's URL and livecheck and `flake.nix` already expect
  `v<version>`, and GitHub's release list reads as one history. `release.yml`
  goes in S7, before the first `v0.11.0*` tag, or that tag would also start a
  Swift build. The updater lanes keep the names `desktop-stable` and
  `desktop-beta` for good, because installed desktop builds have them compiled
  in (`updater.rs`). The old `desktop-v*` tags and releases stay.
  Alternative: keep `desktop-v*`. The history splits, the cask and the flake
  need a second URL scheme, and `release.yml` can wait for S9.
- **D2 The version line continues at 0.11.0.** Candidates are `0.11.0-rc.N`,
  and the stable release is `0.11.0`, above every Swift tag. `1.0.0` remains the
  name for the day the v1 checklist (issue #74, re-scoped to the Rust app) is
  done.
  Alternative: the stable release is `1.0.0`. Read `1.0.0` for `0.11.0`
  throughout; the mechanics do not change.
- **D3 Only the blocking list below gates the stable release.** It blocks on
  what loses data, breaks a core flow (record, transcribe, name, pair, update)
  or fails silently. Everything else follows the release (What follows). One
  item is open: the clip player follows by default; if Nicolai wants it first,
  it joins S4.
  Alternative: the cutover plan's gate, every unticked parity line.
- **D4 The login item stays `SMAppService.mainApp` on macOS.** The shell's
  macOS login item will register, read and remove itself through
  `SMAppService` (the safe `smappservice-rs`, already in `Cargo.lock`), not
  `tauri-plugin-autostart`'s Launch Agent. The Swift registration names the
  bundle (identifier and team), so after the handoff it is expected to keep
  launching the new app; R3 checks that there is one entry and that it starts
  once. Nothing is migrated, and the `requiresApproval` copy stays reachable.
  Linux and Windows keep the plugin.
  Alternative: the cutover plan's step 5 (keep the Launch Agent, unregister
  `SMAppService` on first launch), with its two-item risk.
- **D5 Desktop-id Mac installs convert through their lane.** On macOS only, the
  bundle id becomes `uno.schmid.steno.mac` (`tauri.macos.conf.json`, so the
  Linux and Windows builds keep `uno.schmid.steno.desktop` and their
  directories). Mac installs of `desktop-v0.1.0-rc.*` read `desktop-beta` and
  take `0.11.0-rc.1` as an ordinary update. Each then asks once for microphone,
  system audio and calendar (TCC grants follow the bundle id), and once per
  keychain item it created itself. A Mac that ran both apps from two paths ends
  up with two copies of the Tauri app on one database. The release notes cover
  both.
  Alternative: new lane names for the converted build, which leave those
  installs as a second app on the same database.
- **D6 Linux and Windows ship on every tag; the site lists each once its gate
  passes.** Linux is listed at the stable release if its gate (Order of
  operations) passes. Windows is listed once a Windows machine has run its
  gate.
  Alternative: list both at the stable release.
- **D7 The cask follows stable releases only.** Nicolai sets
  `HOMEBREW_TAP_TOKEN` once, and the workflow bumps the cask; until then each
  bump is a manual tap commit. Candidates are tested from the DMG.
  Alternative: the cask keeps following candidates, as the Swift workflow did.
- **D8 One frozen handoff item.** The `appcast` branch gets one item without a
  channel, for `0.11.0`, with Sparkle's phased rollout
  (`--phased-rollout-interval 86400`: seven groups, one day apart; Check for
  Updates skips the wait). The same feed is also the `appcast.xml` asset of
  every stable release, so `v0.9.0-rc.1` finds it through `releases/latest`.
  After that the branch stops moving. A Swift build that was offline for months
  lands on 0.11.0, and its first daily Tauri check (S4) brings it up to date. A
  later release adds an item only if it fixes the handoff itself. Candidates
  never add one.
  Alternative: every stable release adds its own item until S9 plus three
  months. Sparkle publishing then stays in every stable release.
- **D9 These parity differences are accepted at the handoff.**
  - The optional mixdown stays 16 kHz WAV (the release notes say so).
  - The 64-tap resampler stays.
  - The sidecar's 2 ms lag is accepted.
  - The AAC priming offset of 23 to 48 ms on phone recordings is accepted.
  - Call mode without an output client behaves the same in both apps.
  - The Swift defects under "Store", "Adapters", "Handover", "LLM" and the
    CLI's `--title` close with the handoff, because Swift ships no further
    release, unless a bridge release ships (Release mechanics), which then
    carries them.
  - The fixtures the Swift side owes ("Bridge") are dropped at S9, when the
    Rust fixtures become the contract.
  Alternative: any of these becomes a work package before the stable release.
- **D10 `unsafe` for the Mac's frameworks lives in one new crate,
  `steno-macos`.** Two packages need Mac framework calls that have no safe
  binding: the identity export (`SecItemExport`, S6) and EventKit (S3). The
  crate holds them, each in a safe wrapper with a comment on every invariant,
  and `AGENTS.md`'s `unsafe` rule names it. The shell already has `unsafe`
  (`permissions.rs`, `main.rs`), which the rule does not list; the same PR names
  those two modules too, so the rule matches the code. This changes an
  `AGENTS.md` rule.
  Alternatives:
  - The calls stay in the crates that use them, and the rule lists each
    module.
  - No identity import: phones pair again once (the release notes say so), and
    only EventKit needs the exception. Shelling out to `security export` is not
    an option. `/usr/bin/security` is not on the key's access list, so macOS
    would ask for the login password, and `-t identities` exports every
    identity in the keychain.

## What blocks stable on the Mac

Each row is a **WP9b.** item or an unticked parity line, with the package that
closes it (Work packages). A row is closed when its parity line is ticked or
its item is deleted from "Open after the port".

| Item | Why it blocks | Package |
|---|---|---|
| The Rust app cannot download the CoreML Parakeet model | A fresh Mac install cannot transcribe | S1 |
| Whisper, Ultra and DE have no Rust engine | A Swift user who chose one gets Parakeet v3 in the sidecar, after an unannounced 2.6 GB download | S1 |
| Silent 2.6 GB download inside the pipeline | It fails silently, and it blocks the first Linux release too | S1 |
| Diarizer Settings wording and model licences | Wrong model and size are shown, and WeSpeaker's CC BY 4.0 licence requires attribution | S1 |
| Meeting detection and its prompt (the "Floating panels" line; the recording bubble already shows) | Swift users start most recordings from the prompt | S2 |
| Auto-stop after a call | Recordings run until someone stops them | S2 |
| Calendar lookup at recording start | Meeting titles and attendee names, which seed speaker naming, are lost | S3 |
| Permissions probe, the `unknown` rule, the host's real `Permissions` | A fresh install shows every permission as granted and can record silence | S3 |
| Update schedule (the host's `Updater`) | After the one-way handoff, fixes reach only users who check by hand | S4 |
| QR encoder | A new phone cannot pair | S4 |
| Bonjour re-publish after a network change; the computer name | A laptop that changes network becomes invisible to the phone until Steno restarts | S5 |
| Bundle id, build number, `SUPublicEDKey`, first-launch import | The handoff itself (cutover steps 1, 4, 6; step 5 as D4) | S6 |
| No concurrency group spans the two release workflows | Two macOS signing jobs can run at once | S7 (deletes `release.yml`) |

## What follows

These do not block the stable release. Each keeps or gets an owner line in
"Open after the port" of the Rust plan.

- The tray's badge for pending speaker reviews, the menu bar's queue and five
  recent meetings, and the macOS Record and Find Meetings menu items. The page
  already answers ⌘⇧R and ⌘F, and the main window shows pending reviews.
- The clip player (D3).
- Importing the floating panel's anchor from Swift; the panel opens at its
  default place.
- The whole-lane decode (1.4 GB for a two-hour 48 kHz lane). R5's soak
  measures peak memory on the Mac. If a 16 GB Mac would run short, the decode
  becomes blocking. The Linux gate lists it as a known issue.
- The Swift app's removal (cutover step 7) is S9, after the rollback window.
- The **Unowned.** items in the Rust plan.

## Work packages

Each package is one pull request off `main`, reviewed and merged by merge
commit. Steps marked **Nicolai** need him: secrets, a physical Mac for TCC
prompts and the phone. S1 to S7 are written in parallel.

- **S1 Speech models on the Mac** (`feat/rust-mac-speech-models`).
  - Download the CoreML Parakeet v3 model from the Hugging Face repository
    FluidAudio reads (`FluidInference/parakeet-tdt-0.6b-v3-coreml`), pinned to
    commit `7dd20fe6b1` (the commit Swift installs got) the way
    `PARAKEET_V3_FP32_REVISION` is pinned. The download goes through
    `steno_speech::ModelStore` (resume, lock, mirror) into
    `fluidaudio/parakeet-tdt-0.6b-v3`. It is a new path on an existing host, so
    the PR adds it to invariant 3.
  - When the settings load, map a stored `whisperkit-large-v3-turbo`,
    `parakeet-ultra` or `parakeet-de` to `parakeet-v3`. Show a one-time notice
    ("Steno now transcribes with Parakeet v3"), and drop the three rows from
    Settings.
  - While the running engine's models are missing, processing fails with
    "Download the speech model in Settings" instead of downloading inside the
    pipeline. This is the parity line's second option. "Process again" works
    once the models are installed.
  - The diarizer row's `display_name`, `source_repo` and `expected_bytes`
    describe the ONNX pyannote segmentation 3.0 (MIT) and WeSpeaker ResNet34-LM
    models. `WESPEAKER_RESNET34_LM.licence` becomes CC BY 4.0 (VoxCeleb), from
    the wrong Apache-2.0 in `crates/steno-diarize/src/models.rs`. Both notices
    show with the other acknowledgements, with attribution.
  - Tests:
    - Unit tests for the manifest, the engine mapping and the refusal.
    - On Forge: download into an empty models directory. The file tree and its
      SHA-256 list must equal those of the directory the Swift app installed
      there on 2026-09-25 (23 files, `config.json` and
      `parakeet_v3_vocab.json` included); the list is committed as a fixture.
      The CoreML backend must transcribe the FLEURS sample as before.
- **S2 Detection and auto-stop** (`feat/rust-recorder-policy`).
  - Port `DetectionController`: one prompt at a time, suppressed while
    recording or when the setting is off, shown through `panels::set_prompt`
    with its 60-second countdown, with `dismiss_prompt` reaching the
    controller.
  - Port the auto-stop after a call: the 90-second grace, "Keep recording" and
    the end reasons.
  - Tests: Swift's tests of both policies, ported as table tests against a fake
    clock and a fake process list. **Nicolai**, on his Mac: a FaceTime or Teams
    call raises the prompt, and hanging up stops the recording after the
    grace.
- **S3 Calendar and permissions** (`feat/rust-calendar-permissions`).
  - Look up the overlapping EventKit event at recording start (title and
    attendees), behind a trait whose Mac implementation is in `steno-macos`
    (D10).
  - Add the audio crate's system-audio permission probe, and set the rule for
    `unknown`: off the Mac it counts as not required; on the Mac it counts as
    missing until the probe records one sample. Then wire the host's
    `Permissions` to the shell's `permissions` in place of the fake.
  - Tests: the onboarding opener rule over the four states and three platforms.
    **Nicolai**, in a fresh account: onboarding asks for each permission, and a
    denied system-audio grant shows as missing.
- **S4 Update schedule and QR** (`feat/rust-update-schedule`).
  - The host's `Updater` over `updater.rs`:
    - a daily automatic check, as `SUScheduledCheckInterval` 86400 did;
    - stored automatic-check and automatic-download flags;
    - the stored last check time.
    The General section's Updates row reads them.
  - A QR crate draws the pairing code into the pairing snapshot.
  - Tests: the schedule against a fake clock (a check is due, skipped, retried
    after a failure); the QR image decodes back to the pairing payload (a QR
    decoder as a dev-dependency). R4 proves the schedule on real releases.
- **S5 Handover on a changing network** (`fix/handover-republish`).
  - Re-register the Bonjour record when the interfaces change, on every
    platform.
  - The shell sets `service_name` to the computer name
    (`SCDynamicStoreCopyComputerName` on the Mac, `GetComputerNameExW` on
    Windows).
  - Tests: a fake interface watcher triggers the re-registration.
    **Nicolai**, with a paired phone: switch the Mac to another network and
    back. `dns-sd -B _steno._tcp` shows the record each time, and the phone
    uploads without a restart.
- **S6 The Swift app's identity** (`feat/desktop-mac-identity`).
  - `apps/desktop/src-tauri/tauri.macos.conf.json` sets `identifier` to
    `uno.schmid.steno.mac` (D5).
  - `apps/desktop/src-tauri/Info.plist` carries the Swift `SUPublicEDKey`
    (`RxaX7phoHvb7M0P4yaOC7zngDo+lqlOE6Iq89UtOuQI=`), which is inert in the
    Tauri app.
  - On macOS, `autostart.rs` uses `SMAppService.mainApp` (D4).
  - A macOS-only `swift_import` in `steno-services` runs once, inside the
    shell's `setup` after the single-instance plugin, so a second launch during
    Sparkle's relaunch cannot import twice. Its sources (the defaults domain
    and the keychain) are traits. It is skipped under `STENO_SMOKE_SECONDS` and
    whenever `HOME` is not the account's home, so the smoke never reads the
    real user's data. It does three things:
    - **Preferences.** While `preferences.json` does not exist, copy the keys
      the Rust app reads (`steno.onboardingCompleted`,
      `steno.loginItemRegistered`) through `cfprefsd`
      (`/usr/bin/defaults export uno.schmid.steno.mac -`, parsed with the
      `plist` crate). Copy Sparkle's `SUEnableAutomaticChecks` and
      `SUAutomaticallyUpdate` into S4's flags. Drop the panel anchor.
    - **Handover identity.** While `handover-identity` is empty: the certificate
      by label, `SecIdentityCreateWithCertificate`, `SecItemExport` as PKCS#12,
      then the PEM entry, through `steno-macos` (D10). The Swift item stays in
      place. If the export fails, the app mints a new identity, and the
      release notes say that phones pair again.
    - **Desktop-id leftovers.** Remove the Launch Agent a desktop-id build
      left, and register `SMAppService` instead if that agent was enabled.
  - Tests:
    - A fixture plist covers the import, a missing key and an existing
      `preferences.json`. A second run is a no-op. The smoke skip is asserted.
    - Behind `STENO_KEYCHAIN_TESTS=1`, one test opens a throwaway keychain by
      path. It stores a committed fixture identity there with the calls
      `IdentityKeychain.store` makes (SEC1 `SecItemImport`, certificate add,
      label), and runs the import against that keychain through
      `kSecMatchSearchList`. It never touches the default keychain or the
      search list. The fingerprint and `macID` equal what a Swift test computes
      from the same fixture.
    - Access from one app to another app's items is R3's job.
- **S7 Release mechanics** (`ci/desktop-stable-release`). See Release
  mechanics.
  - Tags move to `v*`. The macOS build gets the commit count as its build
    number.
  - On tag runs only, the macOS bundle job writes the signed handoff item.
    `check-bundle.sh --handoff <build>` asserts the bundle's handoff properties.
  - A stable tag publishes a full release that is GitHub's "latest" and copies
    the appcast as an asset. The approval-gated `handoff` job writes the item;
    the cask is bumped and the flake lines go to the summary.
  - `release.yml` is deleted.
  - Tests: `*.test.sh` for every changed script, each failure of
    `--handoff` included, and a test that a pre-release does not bump the
    cask. `rust-ci.yml` runs every `apps/desktop/scripts/*.test.sh` in a loop
    instead of by name. A manual run on the branch with `platforms=macos`
    passes; it writes no item, since manual runs sign nothing for unpublished
    builds. The first tagged candidate's item is what R2 rehearses.
- **S8 Site and README** (`docs/stable-release-pages`). Written during the
  candidates and merged only after the stable release is public (Landing page
  and README).
- **S9 Swift removal.** Cutover step 7, after the rollback window:
  - the web app moves to `apps/web`;
  - the Swift rows leave `AGENTS.md`;
  - `apps/macos/scripts` goes, with what the desktop workflow still calls
    moved to `apps/desktop/scripts`;
  - issue #74 is re-scoped or closed.

## Release mechanics

### Tags and versions

A stable release is a merge commit that sets `[workspace.package] version` to
`X.Y.Z`, then a pushed `vX.Y.Z` tag. A candidate is the same with `X.Y.Z-rc.N`.
The `plan` job requires the tag to be `v<version>`; today it requires
`desktop-v<version>`. Each tag sits on its own version-bump commit, so no two
tags share a commit count.

The build number is `git rev-list --count HEAD`. The `plan` and macOS jobs check
out with `fetch-depth: 0`. The macOS build gets the number through the
configuration merge (`{"bundle":{"macOS":{"bundleVersion":"<n>"}}}`), which
reaches both the Build and the Bundle step through `$RUNNER_TEMP/tauri-configs`,
as the MSI version does. Before any bundle is built, `plan` fails if the number
is not above the highest `sparkle:version` on the `appcast` branch.

### The workflow after S7

- **Trigger and names.** The trigger becomes `tags: ['v*']`; the `desktop-v*`
  trigger goes. The release title becomes "Steno <version>". The manifest base
  URL that `desktop-release.yml` passes to `updater-manifest.sh`, and the tag
  in the summary's notes, become `releases/download/v<version>`.
- **macOS bundle job.**
  - `check-bundle.sh --signed --handoff <build>` fails unless all of these
    hold:
    - `CFBundleIdentifier` is `uno.schmid.steno.mac`;
    - `SUPublicEDKey` is the Swift key;
    - `CFBundleVersion` equals `<build>`;
    - `codesign --verify -R="=<the committed Swift requirement>"` passes.
  - On tag runs only, after "Notarise the disk image", a "Handoff item" step
    signs. The stapled DMG is final by then, and the EdDSA signature covers
    its bytes. The step:
    - runs `generate_appcast` from the Sparkle 2.10.0 tarball, fetched with a
      SHA-256 pin, with `--ed-key-file -` reading `SPARKLE_PRIVATE_KEY`, which
      is in this step's environment only;
    - passes `--download-url-prefix .../releases/download/v<version>/`,
      `--maximum-deltas 0` and D8's rollout interval;
    - fails unless the output carries `sparkle:edSignature`
      (`generate_appcast` skips signing without an error when the keys do not
      match);
    - dry-runs `merge-appcast.py` against the current `appcast` branch.
    The item carries the build number, the short version, the minimum system
    version and `arm64` from the DMG itself. It goes into its own artifact,
    `sparkle-item`, a name outside the `steno-desktop-*` pattern that `assets`
    downloads, because it is not a release asset.
  - The job's Check secrets gains `SPARKLE_PRIVATE_KEY`, and the desktop
    README's secrets table lists it.
- **`publish`.**
  - A version with a hyphen publishes as today, with `--prerelease
    --latest=false`.
  - A version without one:
    - `gh release edit "$TAG" --draft=false --prerelease=false --latest`;
    - upload the `appcast` branch's `appcast.xml` unchanged as the release's
      `appcast.xml` asset, so `releases/latest/download/appcast.xml` keeps
      serving the handoff item to `v0.9.0-rc.1`;
    - bump the cask with `apps/macos/scripts/bump-homebrew-cask.sh`, which
      falls back to `sha256sum` on Ubuntu;
    - write the Nix flake lines to the job summary.
  - The lane releases stay pre-releases with `--latest=false`.
- **`handoff`** (stable tags only). A job behind the GitHub environment
  `appcast`, whose required reviewer is Nicolai. It runs only while the branch
  has no item without a channel, so in practice once, for 0.11.0. After the
  approval it:
  - runs `apps/macos/scripts/publish-appcast.sh "$TAG" false` with the
    `sparkle-item` artifact;
  - uploads the new `appcast.xml` to the release with `--clobber`.
  Approval follows R7 (Rehearsal). A later release that fixes the handoff
  itself reruns the job by hand, with the repository variable
  `HANDOFF_ITEM_REPLACE` set (D8).
- **`release-notes.sh`.**
  - The opening sentence loses "this macOS build is a preview" and the pointer
    to the latest release.
  - Every `0.11.0-rc.N` and `0.11.0` carry the desktop-id paragraph (D5).
  - `0.11.0` carries the handoff paragraph:
    - Swift users receive this as an update;
    - phones stay paired, or pair again if the identity import failed;
    - the optional mixdown is now WAV;
    - Whisper, Ultra and DE now transcribe with Parakeet v3;
    - Homebrew users can run `brew upgrade --greedy --cask
      nicolaischmid/tap/steno` so brew records the new version.
  - The Windows and OpenPGP paragraphs stay.

### Lanes and "latest"

`desktop-stable` and `desktop-beta` keep their names and rules
(`updater-lanes.sh`). The first stable tag creates `desktop-stable`; no build
reads it before then. An installed `0.1.0-rc.2` reads `desktop-beta` and moves
to `0.11.0-rc.1`.

A stable tag is the only thing that sets `--latest`. From the first stable
release on, `releases/latest` resolves, and so do the site's and the README's
links.

### `release.yml`

S7 deletes it before the first `v0.11.0*` tag. That also closes the
concurrency item. The Swift scripts the desktop workflow still calls
(`publish-appcast.sh`, `merge-appcast.py`, `bump-homebrew-cask.sh`) stay in
`apps/macos/scripts/`, where `ReleaseScriptsTests` and `AppcastScriptsTests`
keep testing them until S9. If the rehearsal shows that a Swift build cannot
take the handoff, a dedicated PR restores `release.yml` with a `swift-v*`
trigger for a bridge release. That release must ship before the handoff item
exists, because the item must carry the highest `sparkle:version` on the
appcast.

### Homebrew and Nix

With the first stable bump, **Nicolai** changes `Casks/steno.rb` once by hand:

- `url ".../releases/download/v#{version}/Steno_#{version}_aarch64.dmg"`;
- `livecheck` takes stable versions only, keeping the anchor:
  `/^v?(\d+(?:\.\d+)+)$/`;
- `zap` adds `~/Library/WebKit/uno.schmid.steno.mac` and
  `~/Library/Application Support/uno.schmid.steno.mac`;
- the comment and the caveat about Sparkle and `releases/latest` say that
  Steno updates itself.

`auto_updates true` stays.

In S7, `flake.nix` gets the same URL, checks for `Contents/MacOS/steno-desktop`
instead of `Steno`, and its UPDATES text names Settings' automatic-check
switch (S4) in place of `defaults write ... SUEnableAutomaticChecks`. `undmg`
unpacks the Tauri DMG (checked on `desktop-v0.1.0-rc.2`). The flake bump stays
a manual PR made from the job summary.

## The Sparkle handoff

### The handoff item

One item, without a channel, served from two places:

- the `appcast` branch (`v0.9.0-rc.2` and later);
- the `appcast.xml` asset of every stable release (`v0.9.0-rc.1`, through
  `releases/latest`).

Its fields:

- `sparkle:version` is the commit count, above 542;
- `sparkle:shortVersionString` is `0.11.0`;
- `minimumSystemVersion` is 15.0, and `hardwareRequirements` is `arm64`;
- the enclosure is
  `https://github.com/NicolaiSchmid/steno/releases/download/v0.11.0/Steno_0.11.0_aarch64.dmg`,
  with its length, `sparkle:edSignature` and `pubDate`, which the phased rollout
  needs.

The Swift items stay on the branch; Sparkle offers the highest build, so they
are never offered again. Installs receive the item on their daily check, one
rollout group per day, or at once from Check for Updates. Nix installs see it
but cannot install from the read-only store; they move by bumping the flake.

### What Sparkle checks

| Check | How the Tauri bundle passes |
|---|---|
| The archive's EdDSA signature verifies against the running app's key | Signed with the same `SPARKLE_PRIVATE_KEY` |
| The new bundle keeps a public key | S6 puts the Swift `SUPublicEDKey` in `Info.plist`; `--handoff` asserts it |
| The new bundle's code signature is valid | Developer ID, team `KQB68F43PW`, hardened runtime, sidecar signed inside, notarised; `check-bundle.sh --signed` |
| The archive holds `Steno.app` | `productName` "Steno" |
| The build number is above the host's | The commit count, checked in `plan` and by `--handoff` |
| Minimum system version and architecture | 15.0 and `arm64`, as in Swift |

Sparkle mounts the DMG, swaps the bundle at the host's path (usually
`/Applications/Steno.app`), deletes the old one and relaunches. The new
executable name (`steno-desktop`) should not matter: Sparkle relaunches the
bundle, and neither TCC nor the keychain names the executable. R2 and R3 check
this.

### First launch after the handoff

| What | How it carries over | Proven by |
|---|---|---|
| Meetings, audio, models | The same `~/Library/Application Support/Steno/`, same schema | R3 |
| Preferences | `swift_import` copies the keys (S6) | R3: onboarding stays closed |
| API key | `keyring`, same service and account, in the file keychain, whose access list should trust the new code through the requirement and the team | R3: a summary runs, no dialog |
| Codex sign-in | The Codex CLI's own `auth.json`, untouched | R3 |
| Handover identity | Imported into `handover-identity` (S6); the Swift item stays | R3: the phone uploads |
| Login item | The Swift `SMAppService.mainApp` registration (D4) | R3: one entry, starts once |
| TCC grants | Same bundle id; the new code should satisfy the stored requirement | R3: no dialog on a call |
| Updates | The Tauri updater reads `desktop-stable` daily (S4); Sparkle's cache stays, harmless | R4 |

The Tauri directories named after the bundle id are the Swift app's
(`~/Library/WebKit/uno.schmid.steno.mac`); the web app keeps nothing there.

## Landing page and README

S8 is merged after R8 passes.

- **`apps/site/src/lib/site.ts`.** `download` stays `releases/latest`, which
  now resolves to a release.
  - `mac`: "Apple Silicon · notarised", listed as released (unchanged).
  - `linux`: ".deb · AppImage · OpenPGP-signed", listed once its gate passes.
  - `win`: "x64 · installer not code-signed", listed once its gate passes.
  The download button needs no change: it already sends a released platform to
  `site.download`.
- **`how-its-built.tsx`.** The subhead loses "is moving". The closing
  paragraph becomes: "Every desktop runs the same Rust app. On the Mac it
  replaced the original Swift app as an ordinary update, with your meetings,
  settings and paired phone where you left them."
- **`open-source.tsx`.** "Swift Mac app · Rust core · Tauri shell" becomes
  "Rust core · Tauri shell".
- **`page.tsx`.** The JSON-LD `operatingSystem` lists the released platforms.
- **README.**
  - The status banner says stable and links the release.
  - Badges: the release badge without `include_prereleases`; Rust CI in place
    of Swift CI; the platform badge names the released desktops.
  - Install, one block per OS:
    - macOS: Homebrew, the DMG and Nix. Updates come through the app. The Nix
      store cannot be written, and Settings' automatic-check switch silences
      the checks.
    - Windows: the MSI or `-setup.exe`. Neither is code-signed, so SmartScreen
      asks first (More info, Run anyway). The hash can be checked against
      `SHA256SUMS`.
    - Linux: `.deb` or AppImage, verified with the `gpg --verify` and
      `sha256sum --check` lines from the desktop README's "Checksums and
      OpenPGP signatures", whose key URL moves from `desktop-v<version>` to
      `v<version>`.
  - "Homebrew details": the tap follows stable releases (D7).
  - "For developers" puts the Rust workspace first; the Swift lines go in S9.
    The rest of the Mac-only wording waits for a docs pass after S9.
- **`apps/desktop/README.md`.**
  - Release, "Cutting a release", "When a run fails", "Publishing, on a tag"
    and the Layout table use `v<version>` and the full release.
  - "A bad release" covers the first stable release (Rollback).
  - "Not here yet" loses what S1 to S6 close.
- **Comments.** The "never latest" comments in `updater.rs` and at the top of
  `desktop-release.yml` go.

## Rehearsal

Nothing reaches Swift users until R1 to R6 and the dogfood have passed on the
last candidate. The stable tag then rebuilds the same code with only the
version and the build number changed. R7 checks that build before Nicolai
approves the handoff item, and R8 checks it through the real feed while the
phased rollout has reached only its first group.

The steps run in two places:

- **Forge** (`ssh forge`), shared with other projects' CI and running every
  runner as the same user, with no `sudo`. Work goes under `~/steno-handoff/`,
  never under `~/steno-calibration/`, and no app goes into `/Applications`.
  Copy bundles with `ditto`. Forge's `gh` is not logged in, so fetch assets
  with `curl` or on atlas.
- **A fresh account on Nicolai's Mac**, a new macOS user. TCC grants,
  keychains and login items are per user, so it behaves like a real install.
  Every Steno build there goes into `~/Applications/Steno.app` and is opened by
  path. Nothing touches `/Applications`, where his daily install lives.

**The local feed.** R2, R3 and the dogfood use a local feed made of the
candidate's `sparkle-item` and DMG. The enclosure URL is rewritten to
`http://localhost:8765/<dmg>`; the EdDSA signature covers the file, not the URL,
and the Swift app does not set `SURequireSignedFeed`. The host is `localhost`
because ATS refuses bare IP addresses on macOS 14 and later. The feed is served
with `python3 -m http.server 8765 --bind localhost`, and the server is stopped
by its recorded PID. Sparkle's log
(`log stream --predicate 'subsystem == "org.sparkle-project.Sparkle"'`) shows
that the feed was read.

**`sparkle-cli`.** It runs the same validator and installer as the in-app
updater, but 2.9.0 removed it from Sparkle's binary download. Build it once on
Forge from the Sparkle `2.10.0` source tag:

```sh
nice -n 19 xcodebuild -project Sparkle.xcodeproj -scheme sparkle-cli \
  -configuration Release CODE_SIGN_IDENTITY=-
```

The product goes into `~/steno-handoff/tools/`, and its `--help` is recorded.
Its installer needs the logged-in session, so each run is a Launch Agent
submitted with `launchctl bootstrap gui/$(id -u) <plist>` (this works from SSH
on Forge), logging to `~/steno-handoff/`, and booted out afterwards. A Swift
release candidate reads `beta`, so the runs pass `--channels beta`.

1. **R1 Desktop-id conversion** (**Nicolai**, fresh account).
   1. Install `desktop-v0.1.0-rc.2`.
   2. Set an API key and turn on launch at login.
   3. When `0.11.0-rc.1` is on `desktop-beta`, check for updates and install.
   Pass when:
   - the app relaunches as `uno.schmid.steno.mac`;
   - Login Items shows one Steno;
   - the TCC prompts appear once, and exactly one keychain prompt appears, for
     the key (D5);
   - the meetings are listed.
2. **R2 Handoff mechanics** (Forge). The cutover plan's test 6.
   1. Save `codesign -d -r- ` of `Steno.app` from the `v0.10.0-rc.2` DMG as
      `swift-dr.txt` (the text after `designated =>`).
   2. For each case, start from a fresh `ditto` copy of the Swift app under
      `~/steno-handoff/apps/`, and run
      `sparkle --feed-url <feed> --check-immediately --channels beta --verbose <app>`.
   3. **Case a.** `v0.10.0-rc.2` (build 542) with the local feed. Pass when:
      - the bundle at that path now has `CFBundleExecutable` `steno-desktop`,
        `CFBundleIdentifier` `uno.schmid.steno.mac`, the candidate's build
        number and the Swift `SUPublicEDKey`;
      - `codesign --verify -R="=$(cat swift-dr.txt)"` and
        `codesign --verify --deep --strict` pass;
      - `spctl -a -t exec -vv` says "Notarized Developer ID".
   4. **Case b.** `v0.10.0-rc.2` with a damaged `edSignature`. The app is
      unchanged.
   5. **Case c.** `v0.10.0-rc.2` with `sparkle:version` 500. No update is found.
   6. **Case d.** `v0.9.0-rc.4` (build 450) with the `appcast` branch's feed
      plus the candidate item. It takes the candidate.
   7. **Case e.** `v0.9.0-rc.1` (build 244), without `--channels`, with the
      same feed served as `/releases/latest/download/appcast.xml`. It takes the
      candidate.
3. **R3 The real handoff** (**Nicolai**, fresh account). The cutover plan's
   tests 1 to 3. The phone keeps one pairing, so pairing it here unpairs his
   daily install until he pairs it there again.
   1. Install `v0.10.0-rc.2`. Grant every permission, set an API key, pair the
      phone, record and process a meeting, and turn on launch at login.
   2. Quit, run `defaults write uno.schmid.steno.mac SUFeedURL
      http://localhost:8765/appcast.xml` (Sparkle 2.10 honours it in release
      builds), and relaunch.
   3. Check for Updates and install.
   4. Pass when:
      - the app comes back as the Tauri app;
      - the meeting is listed;
      - a summary runs with the stored key;
      - onboarding stays closed;
      - Login Items shows one Steno, and it starts once after logging out and
        back in;
      - a call records both lanes;
      - the phone uploads without pairing again;
      - Settings shows the CoreML Parakeet as installed.
      From the relaunch to the phone upload, no keychain or TCC dialog appears.
      Any dialog fails R3: deny it and note which item asked.
   5. Run `defaults delete uno.schmid.steno.mac SUFeedURL`.
4. **R4 Updates after the handoff** (**Nicolai** watches the R3 account). The
   cutover plan's test 4.
   1. Publish `0.11.0-rc.N+1`.
   2. To make the check due now, quit, set S4's stored last-check time back a
      day, and relaunch.
   Pass when the scheduled check finds the new candidate and offers it (or
   installs it, with automatic download on), the app relaunches, and processing
   a meeting starts the sidecar from the new bundle.
5. **R5 Fresh install** (**Nicolai**, a second fresh account; the soak on
   Forge). The cutover plan's test 5.
   1. Open the downloaded, quarantined DMG with networking off:
      `spctl -a -t open --context context:primary-signature -vv` accepts it,
      and the app opens.
   2. Onboarding asks for each permission.
   3. Settings downloads the CoreML Parakeet and the diarizer models with
      progress.
   4. A five-minute call is transcribed, diarized and exported.
   5. On Forge, under `nice` with ONNX threads at 4, the CLI processes a
      two-hour two-lane recording made of FLEURS speech (generated, never
      committed), under `/usr/bin/time -l`. Record the peak resident memory and
      extrapolate to a 16 GB Mac (What follows).
6. **R6 Rollback drill** (**Nicolai**, the R3 account).
   1. `ditto` `v0.10.0-rc.2` back over `~/Applications/Steno.app`.
   2. Pass when:
      - `codesign --verify -R="=<the Tauri requirement>"` passes on it;
      - it opens the same database and lists every meeting, the ones the
        Tauri app recorded included;
      - it reads the key;
      - the phone uploads;
      - Login Items shows one Steno.
   3. Put the Tauri app back.
7. **Dogfood** (**Nicolai**, his own account). After R1 to R6 pass, re-pair the
   phone with his daily install. Then the daily install takes the candidate
   through the local feed, as in R3, and the `SUFeedURL` default is deleted
   afterwards. Pass: at least five real meetings recorded, processed and
   exported over at least three days, one phone upload, and no regression he
   would not ship.
8. **R7 The stable build, before approval** (Forge). Run R2's cases a and e
   against the public `v0.11.0` DMG and its `sparkle-item`. The feed is served
   locally without rewriting the enclosure, so Sparkle downloads the release
   asset itself.
9. **R8 The stable build, through the real feed** (**Nicolai** and atlas).
   Right after the approval:
   - in a fresh account, `v0.10.0-rc.2` takes 0.11.0 through Check for Updates
     with no `SUFeedURL` default. Pass: the app relaunches as 0.11.0, the
     meeting is listed, and no dialog appears;
   - a 0.11.0 install's Check for Updates reports up to date against
     `desktop-stable` (HTTP 200, not an error);
   - `gh api repos/NicolaiSchmid/steno/releases/latest --jq .tag_name` answers
     `v0.11.0`, and `releases/latest/download/appcast.xml` carries the handoff
     item;
   - the `appcast` branch's only item without a channel is 0.11.0, with the
     rollout interval and a `pubDate`;
   - on atlas, the cask's URL with its version filled in downloads a file
     whose SHA-256 matches the cask. **Nicolai**, on his Mac, runs
     `HOMEBREW_NO_AUTO_UPDATE=1 brew install --cask --appdir=~/steno-handoff/apps
     nicolaischmid/tap/steno` in a fresh account, and it installs the Tauri
     app;
   - the flake bump branch builds, and its
     `result/Applications/Steno.app/Contents/Info.plist` names `steno-desktop`
     and 0.11.0.

## Order of operations and gates

1. **Nicolai confirms D1 to D10** on this plan's PR.
2. **S1 to S7 are written in parallel.**
3. **Gate G1.** S1 to S7 are merged, and every row of the blocking list is
   closed.
4. **Bump to `0.11.0-rc.1`.** Once Rust CI is green on the bump commit on all
   three platforms, tag `v0.11.0-rc.1`. This publishes a pre-release and moves
   `desktop-beta`; no item is written. Desktop-id Mac installs convert.
5. **Gate G2.** R1 to R6 and the dogfood pass on the last candidate, in that
   order. Every fix becomes `rc.N+1`, and the steps it touches run again.
6. **Bump to `0.11.0` on a fresh commit and tag `v0.11.0`.** This publishes the
   full release as "latest", creates `desktop-stable`, uploads `appcast.xml`,
   bumps the cask, and puts the flake lines in the summary; open the flake bump
   PR. The `handoff` job waits.
7. **Gate G3.** R7 passes, and Nicolai approves the `handoff` job. R8 passes.
   Merge S8 and the flake bump; Vercel deploys the site from `main`.
8. **Rollback window: 14 days.** No database migration merges. Watch the
   issues. Linux and Windows are listed on the site when their gates pass (D6).
9. **S9**, the Swift removal.

**The Linux gate:**

- S1 and S5 are merged (they are Linux blockers too).
- **Nicolai** runs one GNOME session on hardware: install the `.deb`, record a
  call, log out, and the recording was saved; the AppImage starts.
- The release notes list the open **First Linux release** items as known
  issues: the KDE and Xfce-on-Wayland logout, the whole default sink, no
  meeting detection, the whole-lane decode, and the WebKitGTK descriptor leak.

**The Windows gate:** a Windows machine runs the `--ignored` WASAPI tests and
one real call. Until then the installers are on the release, and the site says
"Not released yet".

## Rollback

**Before step 6**, nothing reaches Swift users or stable installs. A bad
candidate is handled as the desktop README's "A bad release" says for
`desktop-beta`.

**A bad handoff item, while it rolls out:**

1. Revert the item's commit on the `appcast` branch, and re-upload the
   reverted `appcast.xml` to the release. Installs that have not downloaded it
   stop seeing it at their next check. Installs that already downloaded it with
   automatic download on install it when they quit. The phased rollout keeps
   that group small.
2. If fresh installs are affected too, run
   `gh release edit v0.11.0 --prerelease`, which takes the release out of
   "latest". In the same hour, take the affected platform off the site's
   released list.
3. Revert the tap commit and the flake bump.

**Users who already took a bad release** cannot go back through Sparkle. Fix
forward with `0.11.1`, which the daily check (S4, R4) delivers. Stop the
spread on the lanes as the desktop README's "A bad release" says. The first
stable release has no earlier release on `desktop-stable`, so there
`latest.json` is deleted (`gh release delete-asset desktop-stable
latest.json`); S8 adds this to that README section.

**As a last resort for one user**, `ditto` or drag the `v0.10.0-rc.2` app over
the Tauri app. It uses the same database and keychain (R6). Revert the item
first, or the Swift app offers the handoff again.

**The database.** Every migration until S9 is mirrored in `Migrations.swift`,
and none merges in the window, so going back stays possible until the Swift
app is removed.

## Changes to other plans in this plan's PR

- **`.plans/2026-10-04-mac-cutover.md`.** A note at the top says that this plan
  extends it, which parts it replaces, and which risks and test details no
  longer hold.
- **`.plans/2026-10-02-rust-core-and-tauri-shell.md`.**
  - The status line, the WP9b progress row and the owner sentence of "Open
    after the port" point here.
  - The three places that state the old gate (the first **WP9b.** item, the
    "Updates" and "Pending speaker reviews" lines under "Beyond the bridge",
    and seam (4)) say that this plan's blocking list (D3) replaces it.
  - The closed **WP9b.** item about the first `desktop-v*` tag is deleted.
  - The other **WP9b.** items stay until the pull requests that fix them delete
    them.
