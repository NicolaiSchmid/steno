# Stable promotion: the Tauri app becomes the Steno release

Status: planned 2026-10-07, not started; waits for Nicolai to confirm D1 to D10.
Nicolai decided on 2026-10-07 to promote the Tauri app from beta to stable:
release tags and the build pipeline produce the Tauri app, the landing page
offers it, and existing users of the Swift Mac app end up on it without losing
meetings, settings, secrets, paired phones or the login item.

This plan extends `.plans/2026-10-04-mac-cutover.md` and supersedes parts of it:

- the cutover plan's opening gate (every unticked parity line) becomes the
  blocking list below (D3), and its one pull request becomes packages S6 to S9;
- its step 2's beta staging and signing, and all of step 3 (distribution), are
  replaced by "Release mechanics" and "The Sparkle handoff" here; step 2's
  frozen `appcast` branch stands, with one handoff item (D8);
- its open choices get answers: step 1 (D5) and step 5 (D4).

The cutover plan's inventory table, steps 4, 6 and 7, and its tests still apply:
Rehearsal runs tests 1 to 6, R8 runs test 7 and S9 runs test 8. Three details no
longer hold:

- the two-login-items risk, which is moot under D4;
- test 1's premise that a Swift release build reads no channel. Every installed
  build is a release candidate, and from `v0.9.0-rc.2` on each reads `beta`
  too (Facts);
- test 1's check that `codesign -dr -` prints the same text for both apps. The
  texts never match, and `codesign --verify -R` is the check (Facts).

In `.plans/2026-10-02-rust-core-and-tauri-shell.md`, this plan owns every
**WP9b.** item in "Open after the port".

## Facts this plan rests on

Checked on 2026-10-07 against `main` (`e13ab9b8`), the releases, the `appcast`
branch, the released bundles, the GitHub API and the Sparkle 2.10.0 source and
tarball.

- **Every release so far is a pre-release.** Swift: `v0.9.0-rc.1` to
  `v0.10.0-rc.2`. Tauri: `desktop-v0.1.0-rc.1`, `desktop-v0.1.0-rc.2` and the
  rolling `desktop-beta` lane. `GET /releases/latest` answers 404. The site's
  Download button (`site.download` in `apps/site/src/lib/site.ts`) and the
  README's "latest release" link land on the releases list, not on a download.
- **Swift installs read one of two feeds.** Both read with Sparkle 2.10.0.

  | Builds | Feed | Channels |
  |---|---|---|
  | `v0.9.0-rc.1` (build 244), and so every Homebrew install, since the cask is still at that version (`HOMEBREW_TAP_TOKEN` was never set) | `https://github.com/NicolaiSchmid/steno/releases/latest/download/appcast.xml`, a 404 since the first release, so these installs never updated | default only |
  | `v0.9.0-rc.2` to `v0.10.0-rc.2` | the `appcast` branch (`https://raw.githubusercontent.com/NicolaiSchmid/steno/appcast/appcast.xml`) | default and `beta` (`UpdateChannels.allowed`) |

  Every item on the branch carries `beta`, so a beta item would reach every
  install on that feed at once. Staging a handoff on the beta channel holds no
  one back; the rehearsal stages it on a local feed instead.
- **Build numbers.**
  - Sparkle orders updates by `CFBundleVersion` alone, and its installer
    refuses a lower one.
  - The newest Swift build is 542 (`v0.10.0-rc.2`).
  - The Tauri bundle's `CFBundleVersion` is today its marketing version
    ("0.1.0-rc.2"), which Sparkle ranks below 542.
  - `main` has 2068 commits, so the commit count, which is the Swift scheme, is
    far above 542.
  - Tauri 2.10's bundler takes `bundle.macOS.bundleVersion`.
- **What Sparkle checks** is the table under "The Sparkle handoff". Sparkle
  finds the new app by file name (`Steno.app`), with the bundle id only as a
  fallback, and never compares bundle ids. So the bundle id matters for TCC,
  the keychain and the login item, not for Sparkle.
- **Designated requirements.** The Swift app carries Xcode's form of the
  requirement, and the Tauri bundler writes `codesign`'s default form, so the
  two texts never match. TCC and the keychain evaluate whether the new code
  satisfies the stored requirement, which is what `codesign --verify -R` checks.
- **Both desktop pre-releases published without manual steps** (runs
  37349614159 and 37425455820). That covers the `publish` job and an MSI for an
  `-rc.N` version, so the **WP9b.** item about the first `desktop-v*` tag is
  closed.
- **The phone finds the computer by `macID`, not by name.** `macID` is derived
  from the handover certificate's fingerprint (`HandoverIdentity::mac_id`), and
  the phone looks the service up by it (`findByMacID` in
  `mobile/src/features/sync/use-upload-coordinator.ts`). A new identity
  therefore breaks every pairing. A desktop-id build mints and stores its own
  identity at every launch where none exists (`app.rs`, `handover_listener`).
- **Asset names differ.** The Swift release carries `Steno-<v>.dmg`. The Tauri
  release carries:
  - `Steno_<v>_aarch64.dmg` and `Steno_<v>_aarch64.app.tar.gz`;
  - `Steno_<v>_x64_en-US.msi` and `Steno_<v>_x64-setup.exe`;
  - `steno-desktop_<v>_amd64.deb` and `.AppImage`.

  The cask and `flake.nix` build the Swift URL, and the flake also checks for
  an executable named `Steno`.
- **The schemas match.** Swift and Rust both stand at migrations 1 to 4, at
  `v0.10.0-rc.2` and at `main`. A Swift app put back over the Tauri app opens
  the database.

## Decisions to confirm

Each decision states the recommended choice, why, and the alternative. The body
of this plan assumes the recommendation. Where an alternative reaches beyond the
decision's own lines, the alternative says where.

- **D1 Tags are `v*` for the Tauri app.** There is one product, so one tag
  line. The cask's URL and livecheck and `flake.nix` already expect
  `v<version>`, and GitHub's release list then reads as one history.
  `release.yml` goes in S7, before the first `v0.11.0*` tag, or that tag would
  also start a Swift build. The updater lanes keep the names `desktop-stable`
  and `desktop-beta` for good, because installed desktop builds have them
  compiled in (`updater.rs`). The old `desktop-v*` tags and releases stay.
  Alternative: keep `desktop-v*`. The history splits, the cask and the flake
  need a second URL scheme, `release.yml` can wait for S9, and S7 adds a
  concurrency group shared by both workflows for the blocking row.
- **D2 The version line continues at 0.11.0.** Candidates are `0.11.0-rc.N`,
  and the stable release is `0.11.0`, above every Swift tag. `1.0.0` remains the
  name for the day the v1 checklist (issue #74, re-scoped to the Rust app) is
  done.
  Alternative: the stable release is `1.0.0`. Read `1.0.0` for `0.11.0`
  throughout; the mechanics do not change.
- **D3 Only the blocking list below gates the stable release.** It blocks on
  what loses data, breaks a core flow (record, transcribe, name, pair, update)
  or fails silently. The rest follows the release (What follows).
  Alternatives:
  - The cutover plan's gate, every unticked parity line.
  - A smaller change: the clip player joins S4.
- **D4 The login item stays `SMAppService.mainApp` on macOS.** The shell's
  macOS login item will register, read and remove itself through `SMAppService`
  (the safe `smappservice-rs`, already in `Cargo.lock` and moved into
  `[workspace.dependencies]`), not through `tauri-plugin-autostart`'s Launch
  Agent. The Swift registration names the bundle (identifier and team), so
  after the handoff it is expected to keep launching the new app. R3 checks that
  there is one entry and that it starts once. Nothing is migrated, and the
  `requiresApproval` copy stays reachable. Linux and Windows keep the plugin.
  Alternative: the cutover plan's step 5 (keep the Launch Agent and unregister
  `SMAppService` on first launch), with its two-item risk.
- **D5 Mac installs of the desktop-id build (`uno.schmid.steno.desktop`)
  convert through their lane, and the Swift identity wins.**
  - The bundle id becomes `uno.schmid.steno.mac` on macOS only
    (`tauri.macos.conf.json`), so the Linux and Windows builds keep
    `uno.schmid.steno.desktop` and their directories.
  - Mac installs of `desktop-v0.1.0-rc.*` read `desktop-beta` and take
    `0.11.0-rc.1` as an ordinary update. Each then asks once for microphone,
    system audio and calendar, because TCC grants follow the bundle id. It also
    asks once per keychain item it created itself: `handover-identity` at
    launch, and the API key if one was set there.
  - When a Swift handover certificate exists and the import has never run, the
    import replaces the desktop-id identity with the Swift one. Phones paired
    with the Swift app keep working, and a phone paired only with a desktop-id
    build pairs again.
  - A Mac that ran both apps from two paths ends up with two copies of the
    Tauri app on one database.

  The release notes cover all of this.
  Alternatives:
  - The desktop-id identity wins: the import runs only while `handover-identity`
    is empty, and phones paired with the Swift app pair again on such a Mac.
  - New lane names for the converted build. This leaves those installs as a
    second app on the same database.
- **D6 Linux and Windows ship on every tag, and the site lists each once its
  gate passes.** Listing a desktop that has not run on hardware would promise
  something untested. Linux is listed at the stable release if its gate (Order
  of operations) passes. Windows is listed once a Windows machine has run its
  gate.
  Alternative: list both at the stable release.
- **D7 The cask follows stable releases only.** Candidates on brew would move
  Homebrew users onto pre-releases, and `auto_updates true` leaves updates to
  the app anyway. The first stable bump is **Nicolai**'s tap commit (Homebrew
  and Nix). After it he sets `HOMEBREW_TAP_TOKEN`, and the workflow bumps from
  the second stable release on.
  Alternative: the cask keeps following candidates, as the Swift workflow did.
  Then `publish` bumps on every tag, S7 drops its pre-release test, and
  "Homebrew details" in the README keeps its wording.
- **D8 One frozen handoff item.**
  - The `appcast` branch gets one item without a channel, for `0.11.0`, with
    Sparkle's phased rollout (`--phased-rollout-interval 86400`: seven groups,
    one day apart, counted from the approval; Check for Updates skips the
    wait).
  - The same feed is also the `appcast.xml` asset of every stable release, so
    `v0.9.0-rc.1` finds it through `releases/latest`.
  - After that the branch stops moving. A Swift build that was offline for
    months lands on 0.11.0, and its first daily Tauri check (S4) brings it up
    to date.
  - Only a later release that fixes the handoff itself replaces the item.
    Candidates never publish one.

  Alternative: every stable release adds its own item until S9 plus three
  months. Then the `handoff` job runs on every stable tag, R8 checks the newest
  item, and Sparkle publishing stays in every stable release.
- **D9 These parity differences are accepted at the handoff.**
  - The optional mixdown stays 16 kHz WAV, and the release notes say so.
  - The 64-tap resampler stays.
  - The sidecar's 2 ms lag is accepted.
  - The AAC priming offset of 23 to 48 ms on phone recordings is accepted.
  - Call mode without an output client behaves the same in both apps.
  - The Swift defects under "Store", "Adapters", "Handover", "LLM" and the
    CLI's `--title`, and the parity notes' other "before cutover" ports to Swift,
    close with the handoff, because Swift ships no further release. If a bridge
    release ships (Release mechanics), it carries them.
  - The fixtures the Swift side owes ("Bridge") are dropped at S9, when the
    Rust fixtures become the contract.

  Alternative: any of these becomes a work package before the stable release.
- **D10 A new crate, `steno-macos`, joins `AGENTS.md`'s `unsafe` list, and the
  list is brought in line with the code.**
  - Two packages call Mac frameworks that have no safe binding: the identity
    export (`SecItemExport`, S6) and EventKit (S3). `steno-macos` holds both,
    each in a safe wrapper with a comment on every invariant. S6 creates the
    crate, and S3 rebases onto it.
  - The `AGENTS.md` change goes in the same PR: the Rust core row lists the
    crate, and the `unsafe` rule names it.
  - The rule also gains the modules that already hold `unsafe` today:
    - the shell's `permissions.rs` and `main.rs`;
    - `steno-handover`'s `server/advertise.rs` and `upload/receiving_file.rs`;
    - `steno-diarize`'s `coreml/binding.rs`;
    - `steno-speech-sidecar`'s `lib.rs`.
  - S5 reads the computer name through safe routes, so it adds nothing: the
    `system-configuration` crate's `SCDynamicStore` key `Setup:/System` on the
    Mac, and a safe crate on Windows.
  - Shelling out to `security export` is not an option. `/usr/bin/security` is
    not on the key's access list, so macOS would ask for the login password,
    and `-t identities` exports every identity in the keychain.

  Alternatives:
  - The calls stay in the crates that use them, and the rule lists each module.
  - No identity import: phones pair again once (the release notes say so), and
    only EventKit needs the exception.

## What blocks stable on the Mac

Each row is a **WP9b.** item or an unticked parity line, with the package that
closes it. A row is closed when its parity line is ticked or its item is
deleted from "Open after the port".

| Item | Why it blocks | Package |
|---|---|---|
| The Rust app cannot download the CoreML Parakeet model | A fresh Mac install cannot transcribe | S1 |
| Whisper, Ultra and DE have no Rust engine | A Swift user who chose one gets Parakeet v3 in the sidecar after an unannounced 2.6 GB download | S1 |
| Silent 2.6 GB download inside the pipeline | It fails silently; it blocks the first Linux release too | S1 |
| Diarizer Settings wording and model licences | The wrong model and size are shown, and WeSpeaker's CC BY 4.0 licence requires attribution | S1 |
| Meeting detection and its prompt (the "Floating panels" line; the recording bubble already shows) | Swift users start most recordings from the prompt | S2 |
| Auto-stop after a call | Recordings run until someone stops them | S2 |
| Calendar lookup at recording start | Meeting titles and attendee names, which seed speaker naming, are lost | S3 |
| Permissions probe, the `unknown` rule, the host's real `Permissions` | A fresh install shows every permission as granted and can record silence | S3 |
| Update schedule (the host's `Updater`) | After the one-way handoff, fixes reach only users who check by hand | S4 |
| QR encoder | A new phone cannot pair | S4 |
| Bonjour re-publish after a network change, and the computer name | A laptop that changes network becomes invisible to the phone until Steno restarts | S5 |
| Bundle id, build number, `SUPublicEDKey`, first-launch import | The handoff itself (cutover steps 1, 4 and 6, and step 5 as D4) | S6 |
| No concurrency group spans the two release workflows | Two macOS signing jobs can run at once | S7 (deletes `release.yml`) |

## What follows

These do not block the stable release. Each keeps or gets an owner line in
"Open after the port" of the Rust plan.

- The tray's badge for pending speaker reviews. The main window already shows
  pending reviews.
- The menu bar's queue and five recent meetings.
- The macOS Record and Find Meetings menu items. The page already answers ⌘⇧R
  and ⌘F.
- The clip player (D3).
- Importing the floating panel's anchor from Swift. The panel opens at its
  default place.
- The whole-lane decode (1.4 GB for a two-hour 48 kHz lane). R5's soak measures
  peak memory on the Mac, and if a 16 GB Mac would run short, it becomes
  blocking.
- The **Unowned.** items in the Rust plan.

## Work packages

Each package is one pull request off `main`, reviewed and merged by merge
commit. Steps marked **Nicolai** need him: secrets, settings on GitHub, a
physical Mac for TCC prompts, and the phone. S1 to S7 are written in parallel.

- **S1 Speech models on the Mac** (`feat/rust-mac-speech-models`).
  - **The CoreML model.** Download Parakeet v3 from the Hugging Face repository
    FluidAudio reads (`FluidInference/parakeet-tdt-0.6b-v3-coreml`).
    - The download is pinned to commit `7dd20fe6b1`, the one Swift installs got,
      the way `PARAKEET_V3_FP32_REVISION` is pinned.
    - It goes through `steno_speech::ModelStore` (resume, lock, mirror) into
      `fluidaudio/parakeet-tdt-0.6b-v3`.
    - It is a new path on an existing host, so the PR adds it to invariant 3.
  - **The engine mapping.** When the settings load, a stored
    `whisperkit-large-v3-turbo`, `parakeet-ultra` or `parakeet-de` becomes
    `parakeet-v3`. A one-time notice says "Steno now transcribes with Parakeet
    v3", and the three rows leave Settings.
  - **No download inside the pipeline.** While the running engine's models are
    missing, processing fails with "Download the speech model in Settings"
    instead of downloading them itself; this is the parity line's second
    option. "Process again" works once the models are installed.
  - **The diarizer row.** Its `display_name`, `source_repo` and
    `expected_bytes` describe the ONNX models: pyannote segmentation 3.0 (MIT)
    and WeSpeaker ResNet34-LM. `WESPEAKER_RESNET34_LM.licence` in
    `crates/steno-diarize/src/models.rs` changes from the wrong Apache-2.0 to
    CC BY 4.0 (VoxCeleb). Both notices show with the other acknowledgements,
    with attribution.
  - **Tests.**
    - Unit tests for the manifest, the engine mapping and the refusal.
    - On Forge: download into an empty models directory. The file tree and its
      SHA-256 list must equal the tree the Swift app installed there on
      2026-09-25. That tree has 23 files, `config.json` and
      `parakeet_v3_vocab.json` among them, and its hash list is committed as a
      fixture. The CoreML backend then transcribes the FLEURS sample as before.
- **S2 Detection and auto-stop** (`feat/rust-recorder-policy`).
  - Port `DetectionController`:
    - one prompt at a time;
    - suppressed while recording or when the setting is off;
    - shown through `panels::set_prompt` with its 60-second countdown;
    - `dismiss_prompt` reaches the controller.
  - Port the auto-stop after a call: the 90-second grace, "Keep recording" and
    the end reasons.
  - Tests: Swift's `DetectionTests` and `AutoStopTests`, ported as table tests
    against a fake clock and a fake process list. **Nicolai**, on his Mac: a
    FaceTime or Teams call raises the prompt, and hanging up stops the
    recording after the grace.
- **S3 Calendar and permissions** (`feat/rust-calendar-permissions`).
  - Look up the overlapping EventKit event at recording start (title and
    attendees). It sits behind a trait whose Mac implementation is in
    `steno-macos` (D10).
  - Add the audio crate's system-audio permission probe, with this rule for
    `unknown`: off the Mac it counts as not required; on the Mac it counts as
    missing until the probe records one sample. Then the host's `Permissions`
    use the shell's `permissions` in place of the fake.
  - Tests: the onboarding opener rule over the four states and three platforms.
    **Nicolai**, in a fresh account: onboarding asks for each permission, and a
    denied system-audio grant shows as missing.
- **S4 Update schedule and QR** (`feat/rust-update-schedule`).
  - The host's `Updater` over `updater.rs`:
    - a daily automatic check, as `SUScheduledCheckInterval` 86400 did;
    - stored automatic-check and automatic-download flags;
    - the last check time, stored under a file and key the PR names, in RFC 3339
      UTC.

    A check is due at launch and hourly when that time is missing or more than
    24 hours old. The General section's Updates row reads all three.
  - A QR crate draws the pairing code into the pairing snapshot.
  - Tests:
    - the schedule against a fake clock: a check is due at launch, is skipped,
      and is retried after a failure;
    - the QR image decodes back to the pairing payload, with a QR decoder as a
      dev-dependency.

    R4 proves the schedule on real releases.
  - In "Open after the port", S4 cuts the shell's four-gaps item down to the
    badge and the clip player.
- **S5 Handover on a changing network** (`fix/handover-republish`).
  - Re-register the Bonjour record when the interfaces change, on every
    platform.
  - The shell sets `service_name` to the computer name, through the safe routes
    in D10.
  - Tests: a fake interface watcher triggers the re-registration.
    **Nicolai**, with a paired phone: switch the Mac to another network and
    back; `dns-sd -B _steno._tcp` shows the record each time, and the phone
    uploads without a restart.
- **S6 The Swift app's identity** (`feat/desktop-mac-identity`).
  - **Bundle.**
    - `apps/desktop/src-tauri/tauri.macos.conf.json` sets `identifier` to
      `uno.schmid.steno.mac` (D5).
    - `Info.plist` carries the Swift `SUPublicEDKey`
      (`RxaX7phoHvb7M0P4yaOC7zngDo+lqlOE6Iq89UtOuQI=`). It is inert in the Tauri
      app.
    - On macOS, `autostart.rs` uses `SMAppService.mainApp` (D4).
    - The desktop README's identifier paragraph follows the change.
    - The PR creates `steno-macos` and makes the `AGENTS.md` change (D10).
  - **`swift_import`.** A macOS-only import in `steno-services`:
    - It runs inside the shell's `setup`, after the single-instance plugin, so
      a second launch during Sparkle's relaunch cannot import twice.
    - When it has run, it sets `steno.swiftImportRan` in `preferences.json`, and
      it never runs again.
    - Its sources, the defaults domain and the keychain, are traits.
    - It is skipped under `STENO_SMOKE_SECONDS`, and whenever `HOME` is not the
      account's home, so the smoke never reads the real user's data.

    It imports three things:
    - **Preferences.** While `preferences.json` holds no onboarding flag, it
      copies the keys the Rust app reads (`steno.onboardingCompleted`,
      `steno.loginItemRegistered`) through `cfprefsd`
      (`/usr/bin/defaults export uno.schmid.steno.mac -`, parsed with the
      `plist` crate). It copies Sparkle's `SUEnableAutomaticChecks` and
      `SUAutomaticallyUpdate` into S4's flags, and drops the panel anchor.
    - **The handover identity** (D5). It looks the certificate up by label and
      calls `SecIdentityCreateWithCertificate`, then `SecItemExport` as
      PKCS#12, through `steno-macos`, and writes the result to the PEM entry
      `handover-identity`. It replaces a desktop-id identity. The Swift item
      stays in place. If the export fails, the existing or a newly minted
      identity stays, and the release notes say that phones pair again.
    - **Desktop-id leftovers.** It removes the Launch Agent a desktop-id build
      left behind, and registers `SMAppService` instead if that agent was
      enabled.
  - **Tests.**
    - A fixture plist covers the import, a missing key and an existing
      `preferences.json`. A second run is a no-op, and the smoke skip is
      asserted.
    - A keychain test, behind `STENO_KEYCHAIN_TESTS=1`, opens a throwaway
      keychain by path, with user interaction disabled
      (`SecKeychainSetUserInteractionAllowed(false)`, so a prompt fails the
      test instead of showing a dialog):
      - it stores a committed fixture identity with the calls
        `IdentityKeychain.store` makes (SEC1 `SecItemImport`, certificate add,
        label);
      - it runs the import against that keychain through `kSecMatchSearchList`
        for every lookup, with the keychain as
        `SecIdentityCreateWithCertificate`'s first argument and a PKCS#12
        passphrase;
      - it never touches the default keychain or the search list;
      - the fingerprint and `macID` equal what a Swift test computes from the
        same fixture.
    - Access from one app to another app's items is R3's job.
- **S7 Release mechanics** (`ci/desktop-stable-release`). Everything under
  Release mechanics, `release.yml`'s deletion included.
  - Tests:
    - a `*.test.sh` for every changed script, each `--handoff` failure among
      them;
    - `check-bundle.test.sh` stubs `codesign` and `plutil` through `PATH`;
    - a test that a pre-release does not bump the cask;
    - `rust-ci.yml` runs every `apps/desktop/scripts/*.test.sh` in a loop
      instead of by name.
  - A manual run on the branch with `platforms=macos` passes. It signs no
    handoff item, since manual runs sign nothing for unpublished builds, so
    the first tagged candidate's item is what R2 rehearses.
  - The PR deletes the **WP9b.** item that D9 closes.
- **S8 Site and README** (`docs/stable-release-pages`). Written during the
  candidates and merged after R8 passes (Landing page and README).
- **S9 Swift removal.** Cutover step 7, after the rollback window:
  - the web app moves to `apps/web`;
  - the Swift rows leave `AGENTS.md`;
  - `apps/macos/scripts` goes, and what the desktop workflow still calls moves
    to `apps/desktop/scripts`;
  - issue #74 is re-scoped or closed.

## Release mechanics

### Tags and versions

A stable release is a merge commit that sets `[workspace.package] version` to
`X.Y.Z`, then a pushed `vX.Y.Z` tag. A candidate is the same with
`X.Y.Z-rc.N`. A tag is pushed only once Rust CI is green on its bump commit on
all three platforms. The `plan` job requires the tag to be `v<version>`; today
it requires `desktop-v<version>`. Each tag sits on its own version-bump commit,
so no two tags share a commit count.

The build number is `git rev-list --count HEAD`. The `plan` and macOS jobs check
out with `fetch-depth: 0`. The macOS build gets the number through the
configuration merge (`{"bundle":{"macOS":{"bundleVersion":"<n>"}}}`), which
reaches both the Build and the Bundle step through
`$RUNNER_TEMP/tauri-configs`, as the MSI version does. Before any bundle is
built, `plan` fails if the number is not above the highest `sparkle:version` on
the `appcast` branch.

### The workflow after S7

- **Trigger and names.**
  - The trigger becomes `tags: ['v*']`, and the `desktop-v*` trigger goes.
  - The release title becomes "Steno <version>".
  - The manifest base URL that `desktop-release.yml` passes to
    `updater-manifest.sh`, and the tag in the summary's notes, become
    `releases/download/v<version>`.
- **macOS bundle job.**
  - `check-bundle.sh --signed --handoff <build>` fails unless all of these
    hold:
    - `CFBundleIdentifier` is `uno.schmid.steno.mac`;
    - `SUPublicEDKey` is the Swift key;
    - `CFBundleVersion` equals `<build>`;
    - `codesign --verify -R="=$(cat apps/desktop/scripts/swift-designated-requirement.txt)"`
      passes, where that committed file holds the `v0.10.0-rc.2` requirement.
  - On tag runs only, after "Notarise the disk image", a "Handoff item" step
    signs. The stapled DMG is final by then, and the EdDSA signature covers its
    bytes. The step:
    - runs `generate_appcast` from the Sparkle 2.10.0 tarball, fetched with a
      SHA-256 pin;
    - passes `--ed-key-file -` (reading `SPARKLE_PRIVATE_KEY`, which is in this
      step's environment only), `--download-url-prefix
      .../releases/download/v<version>/`, `--maximum-deltas 0` and D8's rollout
      interval;
    - fails unless the output carries `sparkle:edSignature`
      (`generate_appcast` skips signing without an error when the keys do not
      match);
    - dry-runs `merge-appcast.py` against the current `appcast` branch.
  - The item's fields come from the DMG itself (The Sparkle handoff). The item
    goes into its own artifact, `sparkle-item`. The name stays outside the
    `steno-desktop-*` pattern that `assets` downloads, because the item is not
    a release asset.
  - The job's Check secrets gains `SPARKLE_PRIVATE_KEY`, and the desktop
    README's secrets table lists it.
- **`publish`.**
  - A version with a hyphen publishes as today: `--prerelease --latest=false`.
  - A version without one:
    - `gh release edit "$TAG" --draft=false --prerelease=false --latest`;
    - upload the `appcast` branch's `appcast.xml`, read with
      `git fetch origin appcast && git show FETCH_HEAD:appcast.xml`, as the
      release's `appcast.xml` asset;
    - bump the cask with `apps/macos/scripts/bump-homebrew-cask.sh`, as it is
      (`shasum` is on `ubuntu-latest`), once `HOMEBREW_TAP_TOKEN` exists (D7);
    - write the Nix flake lines to the job summary;
    - output whether the branch already has an item without a channel.
  - The lane releases stay pre-releases with `--latest=false`.
- **`handoff`.** This job runs on stable tags only, behind the GitHub
  environment `appcast`, whose required reviewer is Nicolai. Its job-level `if`
  reads `publish`'s output, so it is skipped without asking once the branch has
  its item; in practice it runs once, for 0.11.0. After the approval it:
  - sets the item's `<pubDate>` to the approval time, so the rollout counts
    from then (the EdDSA signature covers only the DMG);
  - runs `apps/macos/scripts/publish-appcast.sh "$TAG" false`, with
    `STENO_RELEASE_APPCAST` pointing at the downloaded `sparkle-item`;
  - uploads the branch's new `appcast.xml`, read as `publish` reads it, to the
    release with `--clobber`.

  Nicolai approves after R7 (Rehearsal). A later release that fixes the handoff
  itself reruns the job by hand with the repository variable
  `HANDOFF_ITEM_REPLACE` set, and the item it writes replaces the old one (D8).
- **`release-notes.sh`.**
  - The opening sentence loses "this macOS build is a preview" and the pointer
    to the latest release.
  - The `SHA256SUMS` line says it lists every file but the OpenPGP signatures
    and `appcast.xml`.
  - Every `0.11.0-rc.N` and `0.11.0` carry the desktop-id paragraph (D5).
  - `0.11.0` carries the handoff paragraph:
    - Swift users receive this release as an update;
    - phones stay paired, or pair again where D5 or a failed import says so;
    - the optional mixdown is now WAV;
    - Whisper, Ultra and DE now transcribe with Parakeet v3;
    - Homebrew users whose app has updated itself can run
      `brew upgrade --greedy --cask nicolaischmid/tap/steno`, so brew records
      the new version;
    - the Linux known issues (The Linux gate).
  - The Windows and OpenPGP paragraphs stay.

### Lanes and "latest"

`desktop-stable` and `desktop-beta` keep their names and their rules
(`updater-lanes.sh`). The first stable tag creates `desktop-stable`, and no
build reads it before then. A stable tag is the only thing that sets
`--latest`. From the first stable release on, `releases/latest` resolves, and
so do the site's and the README's links.

### `release.yml`

S7 deletes it before the first `v0.11.0*` tag, which also closes the
concurrency item. The Swift scripts the desktop workflow still calls stay in
`apps/macos/scripts/` until S9: `publish-appcast.sh`, `merge-appcast.py` and
`bump-homebrew-cask.sh`. There, `ReleaseScriptsTests` and `AppcastScriptsTests`
keep testing them.

If the rehearsal shows that a Swift build cannot take the handoff, a dedicated
PR restores `release.yml` with a `swift-v*` trigger for a bridge release. That
release must ship before the handoff item exists, because the item must carry
the highest `sparkle:version` on the appcast.

### Homebrew and Nix

The first stable bump is one tap commit by **Nicolai**, made after `v0.11.0`
publishes and before `HOMEBREW_TAP_TOKEN` is set. It sets `version` and
`sha256` for 0.11.0, and also:

- `url ".../releases/download/v#{version}/Steno_#{version}_aarch64.dmg"`;
- `livecheck` takes stable versions only and keeps the anchor:
  `/^v?(\d+(?:\.\d+)+)$/`;
- `zap` adds `~/Library/WebKit/uno.schmid.steno.mac` and
  `~/Library/Application Support/uno.schmid.steno.mac`;
- the comment and the caveat about Sparkle and `releases/latest` now say that
  Steno updates itself.

`auto_updates true` stays.

In S7, `flake.nix` changes in three ways:

- it gets the same URL;
- it checks for `Contents/MacOS/steno-desktop` instead of `Steno`;
- its UPDATES text names Settings' automatic-check switch (S4) in place of
  `defaults write ... SUEnableAutomaticChecks`.

`undmg` unpacks the Tauri DMG; this was checked on `desktop-v0.1.0-rc.2`. The
flake bump stays a manual PR made from the job summary.

## The Sparkle handoff

### The handoff item

One item without a channel, served from two places:

- the `appcast` branch, for `v0.9.0-rc.2` and later;
- the `appcast.xml` asset of every stable release, for `v0.9.0-rc.1` through
  `releases/latest`.

Its fields:

- `sparkle:version` is the commit count, above 542;
- `sparkle:shortVersionString` is `0.11.0`;
- `minimumSystemVersion` is 15.0, and `hardwareRequirements` is `arm64`;
- the enclosure is
  `https://github.com/NicolaiSchmid/steno/releases/download/v0.11.0/Steno_0.11.0_aarch64.dmg`,
  with its length and `sparkle:edSignature`;
- the `pubDate` is the approval time, which the phased rollout counts from.

The Swift items stay on the branch. Sparkle offers the highest build, so they
are never offered again. Nix installs see the item but cannot install from the
read-only store, so they move by bumping the flake.

### What Sparkle checks

| Check | How the Tauri bundle passes |
|---|---|
| The archive's EdDSA signature verifies against the running app's key | Signed with the same `SPARKLE_PRIVATE_KEY` |
| The new bundle keeps a public key (Sparkle supports rotating the key, not removing it) | S6 puts the Swift `SUPublicEDKey` in `Info.plist`; `--handoff` asserts it |
| The new bundle's code signature is valid | Developer ID, team `KQB68F43PW`, hardened runtime, sidecar signed inside, notarised (`check-bundle.sh --signed`) |
| The archive holds `Steno.app` | `productName` "Steno" |
| The build number is above the host's | The commit count, checked in `plan` and by `--handoff` |
| Minimum system version and architecture | 15.0 and `arm64`, as in Swift |

Sparkle mounts the DMG, swaps the bundle at the host's path (usually
`/Applications/Steno.app`), deletes the old one and relaunches. The new
executable name (`steno-desktop`) should not matter. Sparkle relaunches the
bundle, and neither TCC nor the keychain names the executable. R2 and R3 check
this.

### First launch after the handoff

| What | How it carries over | Proven by |
|---|---|---|
| Meetings, audio, models | The same `~/Library/Application Support/Steno/`, same schema | R3 |
| Preferences | `swift_import` copies the keys (S6) | R3: onboarding stays closed |
| API key | `keyring`, same service and account, in the file keychain, whose access list should trust the new code through the requirement and the team | R3: a summary runs, no dialog |
| Codex sign-in | The Codex CLI's own `auth.json`, untouched | R3 |
| Handover identity | Imported into `handover-identity` (S6, D5); the Swift item stays | R3: the phone uploads |
| Login item | The Swift `SMAppService.mainApp` registration (D4) | R3: one entry, starts once |
| TCC grants | Same bundle id; the new code should satisfy the stored requirement | R3: no dialog on a call |
| Updates | The Tauri updater reads `desktop-stable` daily (S4); Sparkle's cache stays and is harmless | R4 |

The Tauri directories named after the bundle id are the Swift app's
(`~/Library/WebKit/uno.schmid.steno.mac`). The web app keeps nothing there.

## Landing page and README

S8 is merged after R8 passes.

- **`apps/site/src/lib/site.ts`.** `download` stays `releases/latest`, which
  now resolves to a release.

  | Platform | Note | Listed as released |
  |---|---|---|
  | `mac` | "Apple Silicon · notarised" | yes, as today |
  | `linux` | ".deb · AppImage · OpenPGP-signed" | once its gate passes |
  | `win` | "x64 · installer not code-signed" | once its gate passes |

  The download button needs no change: it already sends a released platform to
  `site.download`.
- **`how-its-built.tsx`.** The subhead loses "is moving". The closing paragraph
  becomes: "Every desktop runs the same Rust app. On the Mac it replaced the
  original Swift app as an ordinary update, with your meetings, settings and
  paired phone where you left them."
- **`open-source.tsx`.** "Swift Mac app · Rust core · Tauri shell" becomes
  "Rust core · Tauri shell".
- **`page.tsx`.** The JSON-LD `operatingSystem` lists the released platforms.
- **README.**
  - The status banner says stable and links the release.
  - The release badge drops `include_prereleases`, Rust CI replaces Swift CI,
    and the platform badge names the released desktops.
  - Install has one block per OS:
    - **macOS:** Homebrew, the DMG and Nix. Updates come through the app. The
      Nix store cannot be written, and Settings' automatic-check switch
      silences the checks.
    - **Windows:** the MSI or the `-setup.exe`. Neither is code-signed, so
      SmartScreen asks first (More info, Run anyway). The hash can be checked
      against `SHA256SUMS`.
    - **Linux:** the `.deb` or the AppImage, verified with the `gpg --verify`
      and `sha256sum --check` lines from the desktop README's "Checksums and
      OpenPGP signatures". The key URL there moves from `desktop-v<version>` to
      `v<version>`.
  - "Homebrew details" says the tap follows stable releases (D7).
  - "For developers" puts the Rust workspace first; the Swift lines go in S9.
  - The rest of the Mac-only wording waits for a docs pass after S9.
- **`apps/desktop/README.md`.**
  - These sections use `v<version>` and the full release: Release, "Cutting a
    release", "When a run fails", "Publishing, on a tag" and the Layout table.
  - "A bad release" covers the first stable release (Rollback).
  - "Not here yet" loses what S1 to S6 close, and marks the clip player as
    following the stable release.
- **Comments.** The "never latest" comments in `updater.rs` and at the top of
  `desktop-release.yml` go.

## Rehearsal

Nothing reaches Swift users until R1 to R6 and the dogfood have passed (G2).
The stable tag then rebuilds the same code with only the version and the build
number changed. R7 checks that build before Nicolai approves the handoff item.
R8 checks it through the real feed, while the phased rollout has reached only
its first group.

The steps run in two places:

- **Forge** (`ssh forge`), which has no `sudo`.
  - Work goes under `~/steno-handoff/`, never `~/steno-calibration/`, and no
    app goes into `/Applications`.
  - `ditto` copies only into a path that does not exist. Where a bundle is
    replaced, remove the old one first, because `ditto` merges into an existing
    directory.
  - Forge's `gh` is not logged in, so assets and artifacts are fetched on atlas
    and copied over.
  - Before R2, `defaults export uno.schmid.steno.mac
    ~/steno-handoff/defaults-before.plist`. After R2, `defaults import` restores
    it, and `~/Library/Caches/uno.schmid.steno.mac/org.sparkle-project.Sparkle`
    is removed. `sparkle-cli` writes the bundle id's real defaults domain.
- **A fresh account on Nicolai's Mac**, a new macOS user per step that says so.
  R4 and R6 reuse R3's account.
  - TCC grants, keychains and login items are per user, so a fresh account
    behaves like a real install.
  - Every Steno build there goes into `~/Applications/Steno.app` and is opened
    by path. Nothing touches `/Applications`, where his daily install lives.
  - Modal keychain or TCC dialogs fail a step; the "Background Items Added"
    notification does not.

**The local feed** is used by R2, R3 and the dogfood.

- It holds the candidate's `sparkle-item` and DMG. The enclosure URL is
  rewritten to `http://localhost:8765/<dmg>`. The EdDSA signature covers the
  file, not the URL, and the Swift app does not set `SURequireSignedFeed`.
- The host is `localhost` because ATS refuses bare IP addresses on macOS 14 and
  later.
- It is served with `python3 -m http.server 8765 --bind 127.0.0.1` in the
  account that runs the step. `curl -fsS http://localhost:8765/appcast.xml`
  must answer before the check.
- Sparkle's log
  (`log stream --predicate 'subsystem == "org.sparkle-project.Sparkle"'`)
  shows that the feed was read.

**`sparkle-cli`** runs the same validator and installer as the in-app updater,
but Sparkle 2.9.0 removed it from the binary download.

- Build it once on Forge, from the Sparkle `2.10.0` tag. Ad-hoc signing is the
  project default and needs no team.

  ```sh
  nice -n 19 xcodebuild -project Sparkle.xcodeproj -scheme sparkle-cli \
    -configuration Release -derivedDataPath ~/steno-handoff/build
  ditto ~/steno-handoff/build/Build/Products/Release/sparkle.app \
    ~/steno-handoff/tools/sparkle.app
  ```

- Its installer needs the logged-in session, so each case is a Launch Agent
  started with `launchctl bootstrap gui/$(id -u) <plist>`, which works from
  SSH on Forge. Each plist has:
  - a unique `Label`;
  - absolute `ProgramArguments`
    (`.../steno-handoff/tools/sparkle.app/Contents/MacOS/sparkle`, the case's
    flags, `--user-agent-name steno-rehearsal`);
  - `RunAtLoad` true and no `KeepAlive`;
  - `StandardOutPath` and `StandardErrorPath` under `~/steno-handoff/logs/`.
    `sparkle` writes to stderr.
- Poll `launchctl print gui/$(id -u)/<label>` until `state = not running`, for
  at most ten minutes, read `last exit code`, then `bootout`.
- Run the cases one at a time. Start the next only when
  `launchctl list | grep uno.schmid.steno.mac-sparkle` is empty, because Sparkle
  resumes an installer still running for the same bundle id.
- A Swift release candidate reads `beta`, so the runs pass `--channels beta`,
  except case e.

### The steps

- **R1 Desktop-id conversion** (**Nicolai**, a fresh account).
  1. Install `desktop-v0.1.0-rc.2`.
  2. Set an API key and turn on launch at login.
  3. When the candidate is on `desktop-beta`, check for updates and install.

  Pass when:
  - the app relaunches as `uno.schmid.steno.mac`;
  - Login Items shows one Steno;
  - the TCC prompts appear once each;
  - one keychain prompt appears per item the desktop-id build created (D5):
    `handover-identity` at launch, then the API key at the first summary.
    After Always Allow, no further prompt appears;
  - the meetings are listed.
- **R2 Handoff mechanics** (Forge; the cutover plan's test 6).
  1. Fetch the candidate's `sparkle-item` on atlas
     (`gh run download <run> -n sparkle-item`) and copy it over.
  2. Each case starts from a fresh `ditto` copy of its Swift app under
     `~/steno-handoff/apps/<case>/` and runs
     `sparkle --feed-url <feed> --check-immediately [--channels beta] --verbose <app>`.
     Every log shows the feed URL being fetched.

  | Case | Swift build | Feed | Pass |
  |---|---|---|---|
  | a | `v0.10.0-rc.2` (542) | the local feed | Exit 0; the checks below |
  | b | `v0.10.0-rc.2` | the local feed with a damaged `edSignature` | Exit 1; the log names the EdDSA validation; the app is unchanged |
  | c | `v0.10.0-rc.2` | the local feed with `sparkle:version` 500 | Exit 4, "No new update available" |
  | d | `v0.9.0-rc.4` (450) | the `appcast` branch's feed plus the candidate item | Exit 0; the candidate is installed |
  | e | `v0.9.0-rc.1` (244), no `--channels` | the same feed | Exit 0; this proves that the item has no channel and suits build 244 (the real route is R8's) |

  Case a's checks:
  - the bundle now has `CFBundleExecutable` `steno-desktop`,
    `CFBundleIdentifier` `uno.schmid.steno.mac`, the candidate's build number
    and the Swift `SUPublicEDKey`;
  - `codesign --verify -R="=$(cat swift-designated-requirement.txt)"` passes,
    with the committed file from S7, and so does
    `codesign --verify --deep --strict`;
  - `spctl -a -t exec -vv` says "Notarized Developer ID".
- **R3 The real handoff** (**Nicolai**, a fresh account; the cutover plan's
  tests 1 to 3). The phone keeps one pairing, so pairing it here unpairs his
  daily install until he pairs it there again.
  1. Install `v0.10.0-rc.2`. Grant every permission, set an API key, pair the
     phone, record and process a meeting, and turn on launch at login.
  2. Quit. Run `defaults write uno.schmid.steno.mac SUFeedURL
     http://localhost:8765/appcast.xml` (Sparkle 2.10 honours it in release
     builds), and relaunch.
  3. Check for Updates and install.
  4. Pass when:
     - the app comes back as the Tauri app;
     - the meeting is listed;
     - a summary runs with the stored key;
     - onboarding stays closed;
     - Login Items shows one Steno, and Steno starts once after logging out
       and back in;
     - a call records both lanes;
     - the phone uploads without pairing again;
     - Settings shows the CoreML Parakeet as installed.

     From the relaunch to the phone upload, no modal dialog appears. If one
     does, deny it and note which item asked.
  5. Run `defaults delete uno.schmid.steno.mac SUFeedURL`.
- **R4 Updates after the handoff** (**Nicolai**, R3's account; the cutover
  plan's test 4).
  1. Publish the next candidate, which changes only the version.
  2. Quit. Set S4's stored last check time to 25 hours ago in the file the S4
     PR names, and relaunch.

  Pass when:
  - the scheduled check finds the new candidate and offers it, or installs it
    if automatic download is on;
  - after the relaunch, `ps -o command= -p $(pgrep -x steno-desktop)` shows
    `~/Applications/Steno.app/Contents/MacOS/steno-desktop`, and About shows the
    new version;
  - with the speech setting switched to the ONNX engine for one meeting,
    `steno-speech-sidecar` starts from the same bundle. Switch the setting
    back.
- **R5 Fresh install** (**Nicolai**, a fresh account; the soak on Forge; the
  cutover plan's test 5).
  1. With networking off, open the downloaded, quarantined DMG.
     `spctl -a -t open --context context:primary-signature -vv` accepts it, and
     the app opens. Turn networking back on.
  2. Onboarding asks for each permission.
  3. Settings downloads the CoreML Parakeet and the diarizer models, with
     progress.
  4. A five-minute call is transcribed, diarized and exported.
  5. On Forge, the CLI processes a two-hour two-lane recording made of FLEURS
     speech (generated, never committed) under `/usr/bin/time -l`. It runs with
     the default four ONNX threads and `HOME=~/steno-handoff/soak-home`, with
     the models copied into its `Library/Application Support/Steno/Models/`,
     no API key and no destination. Record the peak resident memory and
     extrapolate to a 16 GB Mac (What follows).
- **R6 Rollback drill** (**Nicolai**, R3's account).
  1. Save `codesign -d -r-` of the installed Tauri app as `tauri-dr.txt`.
  2. Quit Steno, remove `~/Applications/Steno.app`, and `ditto` the
     `v0.10.0-rc.2` app into its place.
  3. Pass when:
     - `codesign --verify -R="=$(cat tauri-dr.txt)"` passes on it;
     - it opens the same database and lists every meeting, including the ones
       the Tauri app recorded;
     - it reads the key;
     - the phone uploads;
     - Login Items shows one Steno.
  4. Quit, remove the bundle, and `ditto` the Tauri app back.
- **Dogfood** (**Nicolai**, his own account). After R1 to R6 pass:
  1. Re-pair the phone with his daily install.
  2. Check whether his account holds a desktop-id identity
     (`security find-generic-password -s uno.schmid.steno.mac -a handover-identity`).
     If it does, the run also tests D5's rule that the Swift identity wins.
  3. His daily install takes the candidate through the local feed, as in R3,
     and the `SUFeedURL` default is deleted afterwards.

  Pass:
  - at least five real meetings recorded, processed and exported over at
    least three days;
  - one phone upload without pairing again;
  - no regression he would not ship.
- **R7 The stable build, before approval** (Forge).
  1. Fetch `v0.11.0`'s `sparkle-item` on atlas. It has no `sparkle:channel`,
     has `sparkle:phasedRolloutInterval` 86400, and its enclosure is the
     `v0.11.0` asset URL.
  2. Run R2's cases a and e against the public DMG, serving the item locally
     without rewriting the enclosure, so Sparkle downloads the release asset
     itself.
- **R8 The stable build, through the real feed** (**Nicolai** and atlas). Once
  `curl -fsSL https://raw.githubusercontent.com/NicolaiSchmid/steno/appcast/appcast.xml`
  shows the 0.11.0 item:
  - **In two fresh accounts.** `v0.10.0-rc.2` in one and `v0.9.0-rc.1` in the
    other each record one meeting, then take 0.11.0 through Check for Updates,
    with no `SUFeedURL` default. This exercises both real routes. Pass when
    each relaunches as 0.11.0, the meeting is listed, and no dialog appears.
  - **The stable lane.** A 0.11.0 install's Check for Updates reports up to
    date against `desktop-stable`, with HTTP 200 and not an error.
  - **The release.** `gh api repos/NicolaiSchmid/steno/releases/latest --jq
    .tag_name` answers `v0.11.0`, and `releases/latest/download/appcast.xml`
    carries the handoff item.
  - **The appcast branch.** The branch's only item without a channel is
    0.11.0, with the rollout interval and the approval's `pubDate`.
  - **Homebrew** (after Nicolai's tap commit, in his own account):
    1. `git -C "$(brew --repository nicolaischmid/tap)" pull --ff-only`, or
       `brew tap nicolaischmid/tap` if the tap is not there.
    2. `HOMEBREW_NO_AUTO_UPDATE=1 brew fetch --cask nicolaischmid/tap/steno`
       downloads 0.11.0 and verifies its SHA-256.
    3. `brew info --cask nicolaischmid/tap/steno` shows 0.11.0.
    4. The fetched DMG holds `Contents/MacOS/steno-desktop`.
  - **Nix.** The flake bump branch builds, and its
    `result/Applications/Steno.app/Contents/Info.plist` names `steno-desktop`
    and 0.11.0.

## Order of operations and gates

1. **Nicolai confirms D1 to D10** on this plan's PR.
2. **S1 to S7 are written in parallel.**
3. **Gate G1:** S1 to S7 are merged, and every row of the blocking list is
   closed.
4. **Bump to `0.11.0-rc.1` and tag it.** This publishes a pre-release and moves
   `desktop-beta`; no handoff item is signed into the appcast. Desktop-id Mac
   installs convert.
5. **Gate G2.** R1 to R3 pass on candidate N. R4 publishes N+1, which changes
   only the version, and R5, R6 and the dogfood pass on N+1, in that order. A
   fix becomes a new candidate, and the steps it touches run again.
6. **Before the stable tag, Nicolai creates the `appcast` environment** with
   himself as required reviewer, "Prevent self-review" off (he pushes the tag
   himself) and deployment tags `v*`. GitHub would otherwise create the
   environment unprotected, and the item would publish without approval. The
   check: `gh api repos/NicolaiSchmid/steno/environments/appcast --jq
   '[.protection_rules[].type]'` prints `required_reviewers`.
7. **Bump to `0.11.0` on a fresh commit and tag it.** This:
   - publishes the full release as "latest";
   - creates `desktop-stable`;
   - uploads `appcast.xml`;
   - puts the flake lines in the summary.

   The `handoff` job then waits for approval. Nicolai makes the first cask bump
   by hand (D7) and opens the flake bump PR.
8. **Gate G3.** R7 passes. Then Nicolai approves the `handoff` job, and R8
   passes. Merge S8 and the flake bump; Vercel deploys the site from `main`.
   Nicolai sets `HOMEBREW_TAP_TOKEN`.
9. **Rollback window: 14 days.** No database migration merges. Watch the
   issues. Linux and Windows are listed on the site when their gates pass (D6).
10. **S9**, the Swift removal.

**The Linux gate**, on the last candidate or on `v0.11.0`:

- S1 and S5 are merged (they are Linux blockers too).
- **Nicolai**, in one GNOME session on hardware:
  - install the `.deb`;
  - Settings downloads the speech model, with progress;
  - record a call, and check that it is transcribed and exported;
  - log out, and check that the recording was saved;
  - start the AppImage.
- The release notes list the open **First Linux release** items as known
  issues: the KDE and Xfce-on-Wayland logout, the whole default sink, no
  meeting detection, the whole-lane decode, and the WebKitGTK descriptor leak.

**The Windows gate:** on the last candidate or on `v0.11.0`, a Windows machine
runs the `--ignored` WASAPI tests and one real call. Until then the installers
are on the release, and the site says "Not released yet".

## Rollback

**Before step 7**, nothing reaches Swift users or stable installs. A bad
candidate is handled as the desktop README's "A bad release" says for
`desktop-beta`.

**A bad handoff item, while it rolls out:**

1. Revert the item's commit on the `appcast` branch, then run
   `gh release upload v0.11.0 appcast.xml --clobber` with the reverted file.
   Allow five minutes for raw.githubusercontent's cache.
   - Installs that have not downloaded the item stop seeing it at their next
     check.
   - Installs that already downloaded it with automatic download on install it
     when they quit.
   - The phased rollout keeps that group small.
2. If fresh installs are affected too, run `gh release edit v0.11.0
   --prerelease`, which takes the release out of "latest". In the same hour,
   take the affected platform off the site's released list.
   - `gh api repos/NicolaiSchmid/steno/releases/latest` then answers 404.
   - This also cuts off build 244's feed, so step 1's re-upload matters only
     when step 2 is not taken.
3. Revert the flake bump, and Nicolai reverts the tap commit.

**Users who already took a bad release** cannot go back through Sparkle. Fix
forward with `0.11.1`, which the daily check (S4, R4) delivers. Stop the spread
on the lanes as the desktop README's "A bad release" says. The first stable
release has no earlier release on `desktop-stable`, so there `latest.json` is
deleted (`gh release delete-asset desktop-stable latest.json`); S8 adds this to
that README section.

**As a last resort for one user:** quit Steno, then drag the `v0.10.0-rc.2` app
from its DMG onto the Tauri app in Finder and choose Replace. It uses the same
database and keychain (R6). Revert the item first, or the Swift app offers the
handoff again.

**The database.** Every migration until S9 is mirrored in `Migrations.swift`,
and none merges in the window, so going back stays possible until the Swift app
is removed.

## Changes to other plans in this plan's PR

- **`.plans/2026-10-04-mac-cutover.md`.** A note at the top says that this plan
  extends it, which parts it replaces, and which details no longer hold.
- **`.plans/2026-10-02-rust-core-and-tauri-shell.md`.**
  - The status line, the WP9b progress row and the owner sentence of "Open
    after the port" point here.
  - Six places that stated the old gate or the old login-item plan now point
    at this plan:
    - WP9's opening line and its cutover paragraph;
    - the "Updates" and "Pending speaker reviews" lines under "Beyond the
      bridge";
    - seam (4);
    - the first **WP9b.** item;
    - the Shell section's login-item line (D4).
  - The closed **WP9b.** item about the first `desktop-v*` tag is deleted. The
    other **WP9b.** items stay until the pull requests that fix them delete
    them.
