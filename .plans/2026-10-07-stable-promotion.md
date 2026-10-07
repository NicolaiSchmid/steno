# Stable promotion: the Tauri app becomes the Steno release

Status: planned 2026-10-07, not started. Nicolai confirmed D1, D2, D4, D7, D8
and D10 on 2026-10-07 as recommended, and answered D3, D5, D6 and D9 with
changes that this revision carries; the choices those answers open are marked
"to confirm". He also decided that the handover's lost-complete-answer fix
lands before the stable release (D11).

Nicolai decided on 2026-10-07 to promote the Tauri app from beta to stable:
release tags and the build pipeline produce the Tauri app, the landing page
offers it, and existing users of the Swift Mac app end up on it without losing
meetings, settings, secrets, paired phones or the login item. His rule for the
whole promotion: no data is ever lost (D3).

This plan extends `.plans/2026-10-04-mac-cutover.md` and changes it in these
places:

- the opening gate (every unticked parity line) becomes the blocking list below
  (D3), and its one pull request becomes the packages below;
- step 1's bundle id becomes `com.nicolaischmid.steno` (D5), so the app no longer
  keeps the Swift app's id;
- step 2's beta staging and signing, and all of step 3 (distribution), become
  "Release mechanics" and "The Sparkle handoff"; step 2's frozen `appcast`
  branch stands, with one handoff item (D8);
- step 4 reads the Swift app's preference domain explicitly, copies two keys
  and Sparkle's two update flags, and drops the panel anchor (S6);
- step 5's open choice is D4, which keeps `SMAppService`; the new app registers
  itself, and the Swift app's entry is handled as S6 says;
- step 6 also replaces an identity that a `uno.schmid.steno.desktop` build
  stored (D5), and expects a keychain prompt;
- step 7 is S9, apart from `release.yml`, which S7 deletes.

The inventory table and the tests still apply: Rehearsal runs tests 1 to 6, R8
runs test 7 and S9 runs test 8. These details of the cutover plan no longer hold
(Facts):

- the risks that TCC grants and keychain items carry over with the bundle id:
  with a new id they do not, and the plan accepts one re-grant and one keychain
  prompt per item (D5);
- test 1's premise that a Swift release build reads no channel holds only for
  `v0.9.0-rc.1`; every later build is a release candidate and reads `beta` too;
- test 1's check that the designated requirements match: with a new identifier
  they cannot, and Sparkle does not need them to;
- the first risk: Sparkle never refuses a bundle for its id, so only the missing
  key and the signature remain;
- the two-login-items risk becomes the old Login Items entry that S6 handles.

In `.plans/2026-10-02-rust-core-and-tauri-shell.md`, this plan owns every
**WP9b.** item in "Open after the port", and every item there that can lose
data (D3).

## Facts this plan rests on

Checked on 2026-10-07 against `main`, the releases, the `appcast` branch, the
released bundles, the GitHub API, the Sparkle 2.10.0 source and tarball, the
GRDB 7.11.1 source, the Tauri 2.10 bundler source and Omarchy's manual.

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
- **Build numbers.** Sparkle orders updates by `CFBundleVersion` alone, and its
  installer refuses a lower one. The newest Swift build is 542
  (`v0.10.0-rc.2`). The Tauri bundle's `CFBundleVersion` is today its marketing
  version ("0.1.0-rc.2"), which Sparkle ranks below 542. `main` has more than
  2068 commits, so the commit count, which is the Swift scheme, is far above
  542. Tauri 2.10's bundler takes `bundle.macOS.bundleVersion`.
- **Sparkle hands over across bundle ids.** In Sparkle 2.10:
  - the installer finds the new app in the archive by file name (`Steno.app`),
    and by bundle id only as a fallback (`Autoupdate/SUInstaller.m`);
  - the validator has no check that the new bundle's id equals the host's; its
    only id check compares the old host bundle with the id the installer was
    started for (`AppInstaller.m`);
  - with the archive's EdDSA signature verified against the running app's key,
    the new app's code signature only has to be valid, not match the old
    designated requirement (`SUUpdateValidator.m`);
  - the new bundle must still carry a public key: Sparkle supports rotating the
    key, not removing it;
  - the new app lands at the host's path, whatever its id (`SUInstaller.m`).

  R2, R7 and R8 check this on the real bundles.
- **What a new bundle id costs on the Mac** (D5):
  - TCC grants (microphone, system audio, calendar) are stored per bundle id, so
    none carry over and every Swift user grants them once more;
  - the Swift app's keychain items (the API key, the handover identity's key)
    sit in the file keychain, whose access lists trust the Swift app's
    designated requirement, which names `uno.schmid.steno.mac`. The new app is
    in the same team partition (`teamid:KQB68F43PW`) but not on the list, so
    macOS is expected to ask once per item; exporting the identity's private key
    may ask for the login keychain password. R3 records exactly what appears;
  - the `SMAppService.mainApp` registration belongs to the Swift bundle id, and
    an app can only register or unregister itself;
  - the Swift app's preferences stay in the `uno.schmid.steno.mac` domain;
  - Tauri's own directories are named after the identifier (below).
- **Where the identifier names directories.** The support directory, with the
  database, audio and models, is not named after it on any platform
  (`StenoPaths`: `~/Library/Application Support/Steno`, `~/.local/share/Steno`,
  `%APPDATA%\Steno`). Tauri's app config directory, where `panel-anchor.json`
  lives (`panels.rs`), and the webview's data directory are: on macOS
  `~/Library/Application Support/<id>` and `~/Library/WebKit/<id>`, on Linux
  `~/.config/<id>` and `~/.local/share/<id>`, on Windows `%APPDATA%\<id>` and
  `%LOCALAPPDATA%\<id>`. The web app keeps nothing in webview storage. On
  Windows, the MSI upgrade code is derived from the product name and the NSIS
  registry keys use the product name, so an installer with a new identifier
  still upgrades the old install in place; only the NSIS uninstaller's cleanup
  of `%APPDATA%\<id>` and `%LOCALAPPDATA%\<id>` follows the identifier.
- **The keyring service is not the identifier.** Every Steno keyring entry is
  filed under the service string `uno.schmid.steno.mac` (`KEYRING_SERVICE`) on
  every platform. It stays, so no secret moves.
- **`com.nicolaischmid.steno` is also the iOS app's production bundle id**
  (`mobile/app.config.ts`). The two apps are signed differently (App Store and
  Developer ID) and the Mac app needs no provisioning profile, so the account
  allows it. An iOS app is offered on Apple silicon Macs unless App Store Connect
  turns that off; a Mac that ran both would hold two apps with one bundle id,
  sharing TCC grants and confusing Launch Services (D5).
- **Designated requirements.** The Swift app carries Xcode's form of the
  requirement and the Tauri bundler writes `codesign`'s default form. With a new
  identifier the two requirements differ in substance too. Sparkle does not
  compare them (above); `codesign --verify -R` with a saved requirement is the
  check where one is needed (R6).
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
- **The handover listens on a port the system picks** (`port: 0` in
  `HandoverConfiguration`) and advertises through its own mDNS responder
  (`mdns-sd`, UDP 5353). A firewall that blocks incoming connections hides it
  from the phone.
- **Omarchy** (Arch with Hyprland) starts apps through `uwsm`, shows tray icons
  in waybar behind an expander by default, turns `ufw` on with every incoming
  port blocked except 22 and 53317, and creates a passwordless default GNOME
  keyring for its autologin, which an app storing a multi-line secret corrupts
  (Omarchy manual; omacom/omarchy issues #11159 and #8963).
- **Asset names differ.** The Swift release carries `Steno-<v>.dmg`. The Tauri
  release carries `Steno_<v>_aarch64.dmg` and `.app.tar.gz`,
  `Steno_<v>_x64_en-US.msi` and `_x64-setup.exe`, and
  `steno-desktop_<v>_amd64.deb` and `.AppImage`. The cask and `flake.nix` build
  the Swift URL, and the flake checks for an executable named `Steno` and the
  bundle id `uno.schmid.steno.mac`.
- **The schemas match, and an older Swift app opens a newer database.** Swift
  and Rust stand at migrations 1 to 4 on `main`. P2 adds a fifth in both apps.
  GRDB 7.11.1's `DatabaseMigrator.migrate` ignores an applied migration it does
  not know (no error, no erase while `eraseDatabaseOnSchemaChange` is false),
  and the Swift app never calls `hasBeenSuperseded`, so `v0.10.0-rc.2` opens a
  v5 database and leaves the new table alone (R6).

## Decisions

Each decision states the choice, why, and the alternative. The body of this plan
follows the choice.

- **D1 Tags are `v*` for the Tauri app.** Confirmed 2026-10-07. There is one
  product, so one tag line. `release.yml` goes in S7, before the first
  `v0.11.0*` tag. The updater lanes keep the names `desktop-stable` and
  `desktop-beta` for good, because installed desktop builds have them compiled
  in (`updater.rs`). The old `desktop-v*` tags and releases stay.
- **D2 The version line continues at 0.11.0.** Confirmed 2026-10-07. Candidates
  are `0.11.0-rc.N`, and the stable release is `0.11.0`. `1.0.0` remains the
  name for the day the v1 checklist (issue #74, re-scoped to the Rust app) is
  done.
- **D3 Nothing that can lose data ships.** Nicolai, 2026-10-07: "we should
  never loose any data. if there are gaps, then we need to dispatch more
  agents." The gate keeps its shape, with one absolute rule: every known path
  that can lose a recording, a transcript, a note or a pairing blocks the stable
  release, in either app and on every platform listed at the release, whatever
  its likelihood. Everything else that loses no data follows (What follows).
  The blocking list below holds the paths found in the plans; the separate
  audit of the Rust app's code adds its own as P8. The clip player still
  follows; it loses nothing.
- **D4 The login item stays `SMAppService.mainApp` on macOS.** Confirmed
  2026-10-07. The shell's macOS login item registers, reads and removes itself
  through `SMAppService` (the safe `smappservice-rs`, already in `Cargo.lock`,
  moved into `[workspace.dependencies]`), not through `tauri-plugin-autostart`'s
  Launch Agent. With the new bundle id (D5), the Swift registration does not
  carry over; S6 registers the new app and handles the old entry. Linux and
  Windows keep the plugin.
- **D5 The app's identifier becomes `com.nicolaischmid.steno`.** Nicolai,
  2026-10-07: "it should be com.nicolaischmid.steno."
  - **Reading:** one identifier on every platform, replacing both
    `uno.schmid.steno.mac` and `uno.schmid.steno.desktop`, set in
    `tauri.conf.json`. Alternative: per-platform ids (for example the new id on
    the Mac only), which leaves Linux and Windows directories where they are.
  - **What it costs, stated plainly** (Facts): every Swift user grants
    microphone, system audio and calendar once more; macOS asks once per
    keychain item the new app reads (the API key, the handover key); the Swift
    Login Items entry stays behind; desktop-id installs on every platform lose
    Tauri's per-identifier directories, which hold only the panel anchor.
  - **Directories: decouple, do not move** (to confirm). The panel anchor moves
    into the support directory (`Steno/panel-anchor.json`), read once from the
    old Tauri config directory of either old id when the new file is missing.
    Then nothing that matters is named after the identifier, and a later id
    change costs nothing. Webview data starts empty, as it holds nothing.
    Alternative: move the old directories once at first launch, on all three
    platforms.
  - **The iOS app shares the id** (to confirm). Nicolai turns off Mac
    availability for the iOS app in App Store Connect before the stable tag, so
    no Mac runs both. Alternative: the desktop id becomes
    `com.nicolaischmid.steno.desktop`.
  - **Keychain prompts happen once, in one place** (S6). The import reads each
    Swift item once, at the first launch, on an onboarding step that says macOS
    will ask, and writes a new item the new app owns, so no prompt ever appears
    in the middle of a summary or a pairing. The Swift items stay for a
    rollback.
  - **Phones.** When a Swift handover certificate exists and the import has
    never run, the import replaces a desktop-id identity with the Swift one, so
    phones paired with the Swift app keep working; a phone paired only with a
    desktop-id build pairs again. If the export is denied or fails, the phones
    pair again, and the app says so.
  - **Desktop-id installs** on every platform convert through `desktop-beta` at
    `0.11.0-rc.1`. A Mac that ran both apps from two paths ends with two copies
    of one app on one database; the single-instance guard, named after the
    identifier, now covers both.
  - **Mobile's native identifiers are not touched.**
- **D6 Linux targets GNOME, Omarchy and NixOS; Windows ships without being
  listed.** Nicolai, 2026-10-07: "yes. but i also wanna target omarchy and
  nixos users." Every tag publishes all three platforms. The site lists a
  target once its hardware gate passes on Nicolai's machine: GNOME (the `.deb`
  or the AppImage), Omarchy (Arch with Hyprland on Wayland, through an AUR
  package) and NixOS (through the flake). Windows is listed once a Windows
  machine has run its gate.
- **D7 The cask follows stable releases only.** Confirmed 2026-10-07. The first
  stable bump is **Nicolai**'s tap commit (Homebrew and Nix); after it he sets
  `HOMEBREW_TAP_TOKEN`, and the workflow bumps from the second stable release
  on.
- **D8 One frozen handoff item.** Confirmed 2026-10-07.
  - The `appcast` branch gets one item without a channel, for `0.11.0`, with
    Sparkle's phased rollout (`--phased-rollout-interval 86400`: seven groups,
    one day apart, counted from the approval; Check for Updates skips the
    wait).
  - The same feed is also the `appcast.xml` asset of every stable release, so
    `v0.9.0-rc.1` finds it through `releases/latest`.
  - After that the branch stops moving. Only a later release that fixes the
    handoff itself publishes a new item, which supersedes this one.
- **D9 The audio path is final before the first candidate** (to confirm).
  Nicolai, 2026-10-07: "we need to make sure the audio processing is stable and
  ideally the final version before we cutover to the tauri version."
  - **Reading:** no change to the audio and speech path is planned after the
    cutover. Every audio and speech package (A1 to A9) lands before
    `0.11.0-rc.1`, so the rehearsals run on the final pipeline. A fix found
    during the candidates becomes a new candidate and restarts the stability
    count (G2).
  - **The remaining differences, each made final:**

    | Difference | Choice | Why |
    |---|---|---|
    | Mixdown format | **Final: 16 kHz mono WAV** on every platform | One code path and no encoder dependency; the mixdown is optional (`include_audio`). Matching Swift's AAC needs AudioToolbox on the Mac, Media Foundation on Windows and nothing exists for Linux, so the format would differ by platform. Cost: about 115 MB per hour in the vault instead of about 30. Alternative: AAC on the Mac and Windows, WAV on Linux. |
    | Resampler (44.1 kHz phone audio) | **Final as it is**, proven by A9 | Within 0.3 dB to 6 kHz; A9 shows FLEURS word error rates on resampled 44.1 kHz input match the 48 kHz path. |
    | Sidecar's 2 ms lag | **Final, accepted** | Far below a word; Swift had the same relationship. |
    | AAC priming (23 to 48 ms late on phone recordings) | **Fixed first** (A9) | Reading the container's edit list is small and makes the decode exact, as AVFoundation's was. |

  - The Swift defects under "Store", "Adapters", "Handover", "LLM" and the
    CLI's `--title`, and the parity notes' other "before cutover" ports to Swift,
    close with the handoff, because Swift ships no further release. If a bridge
    release ships (Release mechanics), it carries them.
  - The fixtures the Swift side owes ("Bridge") are dropped at S9, when the
    Rust fixtures become the contract.
- **D10 A new crate, `steno-macos`, joins `AGENTS.md`'s `unsafe` list, and the
  list is brought in line with the code.** Confirmed 2026-10-07.
  - Two packages call Mac frameworks that have no safe binding: the identity
    export (`SecItemExport`, S6) and EventKit (S3); the import's other keychain
    calls use `security-framework`'s safe wrappers. `steno-macos` holds both,
    each in a safe wrapper with a comment on every invariant, and also the
    keychain test's fixture calls (`SecItemImport` of SEC1, `SecItemUpdate`).
    S6 creates the crate, and S3 rebases onto it.
  - The `AGENTS.md` change goes in the same PR: the Rust core row lists the
    crate, and the `unsafe` rule names it and the modules that already hold
    `unsafe` today: the shell's `permissions.rs` and `main.rs`;
    `steno-handover`'s `server/advertise.rs` and `upload/receiving_file.rs`;
    `steno-diarize`'s `coreml/binding.rs`; `steno-speech-sidecar`'s `lib.rs`;
    the test targets `steno-speech-sidecar/tests/isolation.rs` and
    `steno-speech/tests/frames.rs`.
- **D11 The handover's lost-complete-answer fix lands before the stable
  release.** Decided by Nicolai on 2026-10-07. It is P2, with a fifth migration
  in both apps; the 14-day no-migration rule applies after the stable tag.
- **D12 The stability gate is ten real meetings on the Mac and five on each
  Linux target** (to confirm). See G2. Alternative: more meetings, or a number
  of days of daily use.

## What blocks the stable release

Each row closes when its parity line is ticked or its item is deleted from "Open
after the port", and its PR is merged. Rows marked with a PR number are already
open.

**On the Mac, for the handoff:**

| Item | Why it blocks | Package |
|---|---|---|
| The Rust app cannot download the CoreML Parakeet model | A fresh Mac install cannot transcribe | S1 |
| Whisper, Ultra and DE have no Rust engine | A Swift user who chose one gets Parakeet v3 after an unannounced 2.6 GB download | S1 |
| Silent 2.6 GB download inside the pipeline | It fails silently | S1 |
| Diarizer Settings wording and model licences | The wrong model is shown, and WeSpeaker's CC BY 4.0 licence requires attribution | S1 |
| Meeting detection and its prompt | Swift users start most recordings from the prompt; a missed meeting is a lost recording | S2 |
| Auto-stop after a call | Recordings run until someone stops them | S2 |
| Calendar lookup at recording start | Meeting titles and attendee names are lost | S3 |
| Permissions probe and the real `Permissions` | A fresh install, and every Swift user after the new id, can record silence | S3 |
| Update schedule | After the one-way handoff, fixes reach only users who check by hand | S4 |
| QR encoder | A new phone cannot pair | S4 |
| Bonjour re-publish after a network change, and the computer name | The phone loses the computer until Steno restarts | S5 |
| The new identifier, build number, `SUPublicEDKey`, first-launch import, login item | The handoff itself | S6 (build number: S7) |
| No concurrency group spans the two release workflows | Two macOS signing jobs can run at once | S7 (deletes `release.yml`) |

**Everywhere, because they can lose data (D3):**

| Item | What can be lost | Package |
|---|---|---|
| The intake's receipt and meeting commits run under `synchronous = NORMAL` | A recording: a power loss after `complete` can roll back the meeting while the phone has deleted its copy | P1 (#213) |
| A `complete` answer that never reaches the phone | A recording the phone keeps resending, or a second meeting; a recording whose meeting was deleted on the computer must be answered "delivered" | P2 (D11) |
| A recording ended by a crash, a kill, a power loss or a logout cut short | The recording, unless its audio up to the last flush survives and processes at the next launch | P3 |
| A stop that waited can lose its turn (Audio parity notes) | A recording's stop, so the caller never gets the failed recording | P4 |
| A logout or shutdown save that outlasts the session manager's or logind's wait, on every Linux desktop and on Windows | The recording in progress | P5 (with #220) |
| A people folder typed as `./People` or `.` in the Swift Settings | Every note: each Rust delivery fails with `PeopleFolderInvalid` | P6 |
| A multi-line secret in Omarchy's passwordless keyring (the handover identity is a multi-line PEM) | Every stored secret and the pairing | P7 (with #221) |
| A headset that switches to 24 or 16 kHz, or a device that will not run at 48 kHz | The recording: it fails to start, or ends | A6 (#198) |
| The whole-lane decode (several GB for a two-hour two-channel master) | A processing run, and any recording in progress if the app runs out of memory | A1 |
| The diarizer's ONNX inference in the app's process | Any recording in progress if ONNX Runtime crashes | A3 |
| A pipeline reload that leaves two speech sidecars (about 4.4 GB) | As above, through memory | A5 (#218) |
| PipeWire's `stop()` hang | The recording that never finalises | A4 (#214) |
| The gaps the code audit finds | Whatever it names | P8 |

**For the Linux targets (D6):** X1 to X6 and the hardware gates.

**Also landing before the first candidate,** losing nothing but changing the
code the rehearsals run: #219 and #212 (the handover leaves other devices'
files alone), #215 (the folder note names the platform), #217 (no title-bar band
off the Mac).

## What follows

These lose no data and do not block. Each keeps or gets an owner line in "Open
after the port".

- The tray's badge for pending speaker reviews; the main window shows them.
- The menu bar's queue and five recent meetings.
- The macOS Record and Find Meetings menu items; the page answers ⌘⇧R and ⌘F.
- The clip player.
- The **Unowned.** items that lose nothing: the CoreML backend's own chunker is
  A2 now; the speech settings without a Settings row; the diarizer's model
  store is A3 now; the untested `steno-llm` cleanup paths.

## Work packages

Each package is one pull request off `main`, reviewed and merged by merge
commit. Steps marked **Nicolai** need him: secrets, settings on GitHub and App
Store Connect, his machines for TCC prompts and hardware, and the phone. Every
package below is written in parallel; G1 waits for all of them.

### For the Mac and the handoff

- **S1 Speech models on the Mac** (`feat/rust-mac-speech-models`).
  - **The CoreML model.** Download Parakeet v3 from the Hugging Face repository
    FluidAudio reads (`FluidInference/parakeet-tdt-0.6b-v3-coreml`), pinned to
    commit `7dd20fe6b1` the way `PARAKEET_V3_FP32_REVISION` is pinned, through
    `steno_speech::ModelStore` (resume, lock, mirror) into
    `fluidaudio/parakeet-tdt-0.6b-v3`. It is a new path on an existing host, so
    the PR adds it to invariant 3.
  - **The engine mapping.** A stored `whisperkit-large-v3-turbo`,
    `parakeet-ultra` or `parakeet-de` becomes `parakeet-v3` when the settings
    load. A one-time notice says "Steno now transcribes with Parakeet v3", and
    the three rows leave Settings.
  - **No download inside the pipeline.** While the running engine's models are
    missing, processing fails with "Download the speech model in Settings".
    "Process again" works once they are installed.
  - **The diarizer row** describes the ONNX models: pyannote segmentation 3.0
    (MIT) and WeSpeaker ResNet34-LM, whose `licence` in
    `crates/steno-diarize/src/models.rs` changes from the wrong Apache-2.0 to
    CC BY 4.0 (VoxCeleb). Both notices show with attribution.
  - **Tests.** Unit tests for the manifest, the mapping and the refusal. On
    Forge, a download into an empty models directory gives the file tree and
    SHA-256 list of the tree the Swift app installed there on 2026-09-25 (23
    files, committed as a fixture), and the CoreML backend transcribes the
    FLEURS sample as before.
- **S2 Detection and auto-stop** (`feat/rust-recorder-policy`).
  - Port `DetectionController`: one prompt at a time, suppressed while
    recording or when the setting is off, shown through `panels::set_prompt`
    with its 60-second countdown, with `dismiss_prompt` reaching the controller.
  - Port the auto-stop after a call: the 90-second grace, "Keep recording" and
    the end reasons.
  - Tests: Swift's `DetectionTests` and `AutoStopTests` as table tests against a
    fake clock and a fake process list. **Nicolai**, on his Mac: a FaceTime or
    Teams call raises the prompt, and hanging up stops the recording after the
    grace.
- **S3 Calendar and permissions** (`feat/rust-calendar-permissions`).
  - Look up the overlapping EventKit event at recording start, behind a trait
    whose Mac implementation is in `steno-macos` (D10).
  - Add the system-audio permission probe, with this rule for `unknown`: off the
    Mac it counts as not required; on the Mac it counts as missing until the
    probe records one sample. The host's `Permissions` then use the shell's
    `permissions`. After the handoff, the missing grants open onboarding on its
    permissions page, whatever `steno.onboardingCompleted` says.
  - Tests: the opener rule over the four states and three platforms.
    **Nicolai**, in a fresh account: onboarding asks for each permission, and a
    denied system-audio grant shows as missing.
- **S4 Update schedule and QR** (`feat/rust-update-schedule`).
  - The host's `Updater` over `updater.rs`: a daily automatic check, stored
    automatic-check and automatic-download flags, and the last check time,
    stored under a file and key the PR names, in RFC 3339 UTC. A check is due at
    launch and hourly when that time is missing or more than 24 hours old. The
    General section's Updates row reads them. A packaged install (X4) shows
    "Updates come from your package manager" instead.
  - A QR crate draws the pairing code into the pairing snapshot.
  - Tests: the schedule against a fake clock; the QR image decodes back to the
    pairing payload. R4 proves the schedule on real releases.
  - In "Open after the port", S4 cuts the shell's four-gaps item down to the
    badge and the clip player.
- **S5 Handover on a changing network** (`fix/handover-republish`).
  - Re-register the Bonjour record when the interfaces change, on every
    platform.
  - The shell sets `service_name` to the computer name through a safe route on
    the Mac and Windows (Linux keeps the host name), for example
    `whoami::devicename()`.
  - Tests: a fake interface watcher triggers the re-registration.
    **Nicolai**, with a paired phone: switch the computer to another network and
    back; `dns-sd -B _steno._tcp` (or `avahi-browse -rt _steno._tcp` on Linux)
    shows the record each time, and the phone uploads without a restart.
- **S6 The new identifier and the import** (`feat/desktop-identifier`).
  - **Identifier.** `tauri.conf.json` sets `identifier` to
    `com.nicolaischmid.steno` (D5). `Info.plist` carries the Swift
    `SUPublicEDKey` (`RxaX7phoHvb7M0P4yaOC7zngDo+lqlOE6Iq89UtOuQI=`), inert in
    the Tauri app. `panel-anchor.json` moves into the support directory, read
    once from either old config directory. The desktop README's identifier
    paragraph follows. The PR creates `steno-macos`, makes the `AGENTS.md`
    change (D10), and adds `security-framework`, `plist` and the PKCS#12 crate
    to `[workspace.dependencies]`.
  - **Login item** (D4). On macOS, `autostart.rs` uses `SMAppService.mainApp`.
    At the first launch after the handoff the new app registers itself when the
    stored `launch_at_login` setting (shared database) is on. The Swift entry
    belongs to `uno.schmid.steno.mac`, which no app can unregister for another.
    What it does after the handoff decides the handling, and R3 finds out:
    - if it launches nothing or disappears, nothing more is done;
    - if it stays in Login Items as a second "Steno", the release notes and the
      onboarding step tell the user to remove it in System Settings > General >
      Login Items;
    - if it launches the new app, the single-instance guard keeps one running,
      and the same note applies.
    If R3 shows a worse outcome, the fallback is a Swift bridge release whose
    Sparkle delegate unregisters the Swift item before installing (Release
    mechanics).
  - **`swift_import`.** A macOS-only import in `steno-services`:
    - It runs first in the shell's `setup`, before `Host::real` builds the
      graph, because `handover_listener` loads or mints `handover-identity`
      there and the host reads the onboarding and login-item flags. The
      single-instance plugin has already let a second launch exit by then.
    - When it has run, it sets `steno.swiftImportRan` in `preferences.json`, and
      it never runs again. Its sources (the defaults domain and the keychain)
      are traits. It is skipped under `STENO_SMOKE_SECONDS` and whenever `HOME`
      is not the account's home.
    - **Preferences.** While `preferences.json` holds no onboarding flag, it
      reads the Swift domain explicitly
      (`/usr/bin/defaults export uno.schmid.steno.mac -`, parsed with the
      `plist` crate), copies `steno.onboardingCompleted` and
      `steno.loginItemRegistered`, copies Sparkle's `SUEnableAutomaticChecks`
      and `SUAutomaticallyUpdate` into S4's flags, and drops the panel anchor.
    - **Secrets, on one onboarding step.** The step says that macOS will ask
      once to let the new Steno read what the old one stored, then reads the
      API key through `keyring` and writes it again, so the new item belongs to
      the new app. A denied read leaves the key empty, and Settings asks for it.
    - **The handover identity** (D5). It looks the certificate up by label,
      calls `SecIdentityCreateWithCertificate` through `security-framework`,
      then `SecItemExport` as PKCS#12 through `steno-macos`. A PKCS#12 crate
      (for example `p12-keystore`, which reads Apple's legacy encryption;
      `cargo deny` must allow it) decodes it into the PEM entry
      `handover-identity`, replacing a desktop-id identity. The export is
      expected to prompt, possibly for the login password. If it is denied or
      fails, the existing or a new identity stays, and the step says that phones
      pair again. The Swift items stay in place.
    - **Desktop-id leftovers.** It removes the Launch Agent a desktop-id build
      left behind, and registers `SMAppService` instead if that agent was
      enabled.
  - **Tests.**
    - A fixture plist covers the import, a missing key and an existing
      `preferences.json`. A second run is a no-op, and the smoke skip is
      asserted.
    - A keychain test, behind `STENO_KEYCHAIN_TESTS=1` and run by the S6 author
      on Forge before the merge, opens a throwaway keychain by path with user
      interaction disabled. It stores a committed fixture identity with the
      calls `IdentityKeychain.store` makes, runs the import against it through
      `kSecMatchSearchList` (with the keychain as
      `SecIdentityCreateWithCertificate`'s first argument and a PKCS#12
      passphrase), never touches the default keychain, and gets the fingerprint
      and `macID` a Swift test computes from the same fixture.
    - Access across apps, the prompts and the login item are R3's job.
- **S7 Release mechanics** (`ci/desktop-stable-release`). Everything under
  Release mechanics, `release.yml`'s deletion included.
  - Tests: a `*.test.sh` for every changed script, each `--handoff` failure
    among them, with `check-bundle.test.sh` stubbing `codesign` and `plutil`;
    a test that a pre-release does not bump the cask; `rust-ci.yml` runs every
    `apps/desktop/scripts/*.test.sh` in a loop; the `pubDate` rewrite's test
    parses the value back with Sparkle's format.
  - The PR removes the three `StenoTests` that read `release.yml` (one in
    `AppcastScriptsTests.swift`, two in `ReleaseScriptsTests.swift`). Their
    still-relevant assertions move to a `desktop-release.yml` check: `publish`
    makes the release public and uploads `appcast.xml` before the cask bump,
    `handoff` needs `publish`, and `contents: write` is set. The PR touches
    `apps/macos/`, so `swift-ci` runs on it.
  - A manual run with `platforms=macos` passes. It signs no handoff item, so the
    first tagged candidate's item is what R2 rehearses.
  - The PR deletes the **WP9b.** item that D9 closes.
- **S8 Site and README** (`docs/stable-release-pages`). Written during the
  candidates and merged after R8 passes.
- **S9 Swift removal.** Cutover step 7, after the rollback window: the web app
  moves to `apps/web`, the Swift rows leave `AGENTS.md`, `apps/macos/scripts`
  goes (what the desktop workflow still calls moves to `apps/desktop/scripts`),
  and issue #74 is re-scoped or closed.

### The final audio path (D9)

Each lands before `0.11.0-rc.1`. Several are open already.

- **A1 A streamed decoder** (`fix/audio-streamed-decode`). Decode every lane in
  bounded chunks, CAF and WAV included, so a two-hour two-channel 48 kHz master
  never sits in memory whole. Tests: peak memory of a synthetic two-hour master
  stays under a bound the PR names; decoded samples equal today's.
- **A2 CoreML on the shared chunker** (`refactor/coreml-shared-chunker`). The
  CoreML backend moves onto the shared chunker, merge and decoder settings,
  settling the unticked WP4 integration notes. Parity: FLEURS and the Swift
  fixtures hold within the same tolerance as today.
- **A3 The diarizer on `ModelStore` and in the sidecar**
  (`feat/diarizer-in-sidecar`). The diarizer's models come through
  `steno_speech::ModelStore` (resume, lock, mirror), and its inference runs in
  the speech sidecar through a request of its own, so a crash in ONNX Runtime
  ends the child, not the app (invariant 4). Parity: the diarization fixtures
  give the same clusters.
- **A4 PipeWire** (#214): `stop()` bounded, the own-output question and the
  default-move bug settled; plus the PipeWire differences that remain (`start`
  waiting for the first cycle, latencies measured on Nicolai's hardware).
- **A5 One speech engine per reload** (#218).
- **A6 Devices that will not run at 48 kHz** (#198).
- **A7 The Linux input device list** (`feat/pipewire-device-list`):
  Settings > Recording lists PipeWire's input nodes, and a chosen node survives a
  restart.
- **A8 Meeting detection on Linux** (`feat/linux-meeting-detection`): detection
  by the PipeWire streams of known call apps, behind the same controller as S2.
- **A9 Final choices proven** (`fix/audio-final-choices`): the AAC priming offset
  is trimmed from the container's edit list; FLEURS word error rates on
  resampled 44.1 kHz input match the 48 kHz path within 0.1 points; the 2 ms
  sidecar lag stays pinned by `tests/codec.rs`.

### No data lost (D3)

- **P1 Durable intake commits** (#213).
- **P2 The lost complete answer** (`fix/handover-lost-complete-answer`, D11). A
  fifth migration in both apps adds the `handoverAdmission` table, so a
  `complete` whose answer was lost is answered "delivered" when the phone asks
  again, and a recording whose meeting was deleted on the computer is answered
  "delivered" too. The PR states what the Rust app does with an admission the
  rolled-back Swift app made without a row (R6), and the schema parity test
  covers v5.
- **P3 Recovery of an interrupted recording** (`fix/recording-recovery`). A
  recording ended by a kill, a crash, a power loss or a logout cut short keeps
  its audio up to the writer's last flush (the PR names the interval) and
  becomes a meeting that processes at the next launch; today such a recording is
  marked failed at launch, and the PR proves its audio is kept and processable,
  or makes it so. Tests: `kill -9` at the start, the middle and during the stop
  of a recording on each platform, then a launch and Process again.
- **P4 A stop never loses its turn** (`fix/recorder-stop-turn`). A `stop()` that
  waited behind a writer failure's or a device loss's finalise returns that
  recording, as Swift's actor did. Test: the interleaving forced with a gate.
- **P5 Saves within the session's wait** (with #220). Measure the save time of a
  two-hour recording at quit; if it can exceed logind's five seconds,
  gnome-session's ten or xfce4-session's seven, the save first writes what makes
  the recording recoverable (P3) and finishes the rest at the next launch. On
  Windows, `ShutdownBlockReasonCreate` while a recording runs (Windows gate).
- **P6 The people folder** (`fix/adapters-people-folder`): the Rust delivery
  accepts a stored `./People` or `.` by normalising it once to the plain
  relative form, so no note fails.
- **P7 Secrets that survive Omarchy's keyring** (with #221): every secret written
  to the Secret Service is a single line (the PEM bundle base64-encoded, read
  back either way), and #221's migration writes that form.
- **P8 The code audit's gaps.** The separate audit of the Rust app's code adds
  its data-loss findings here, one package each, before G1.

### For the Linux targets (D6)

- **X1 Session end under Hyprland** (with #220). On Omarchy a logout ends
  Hyprland, which ends XWayland: #220's lost-display path saves first. Under
  `uwsm`, the stop of the graphical session's units sends SIGTERM, which waits
  for the save. A shutdown or reboot uses logind's delay lock. xdg-desktop-portal-
  hyprland serves no session monitor or logout inhibitor, and the plan relies on
  neither. Tests on Nicolai's Omarchy machine (Linux gates).
- **X2 Panels and tray under Hyprland** (`fix/hyprland-panels-tray`). The
  floating panels must float, stay on top and keep their place under Hyprland's
  XWayland: the shell sets a window type Hyprland floats (utility), and if
  Hyprland still tiles them, the AUR package and the README ship the
  `windowrule` lines. The tray works through waybar's SNI module; Omarchy hides
  it behind the expander, which the README says. Without a tray host the app
  behaves as today (closing the main window quits).
- **X3 The handover behind a firewall** (`feat/handover-fixed-port`). A fixed
  default handover port (configurable, the PR picks one outside the IANA
  registered range) replaces `0` on Linux, with `0` kept as the fallback when it
  is taken and a warning in Settings. Omarchy's `ufw` gets an application
  profile from the AUR package (`ufw allow Steno` opens the port and UDP 5353),
  and the NixOS module opens both.
- **X4 Packaged installs** (`feat/packaged-install`). A build installed by
  pacman or Nix turns the in-app updater off (a marker the package writes beside
  the binary), and Settings says updates come from the package manager. On NixOS
  the autostart entry points at a stable path
  (`/run/current-system/sw/bin/steno-desktop` or the user profile's), never at a
  store path that garbage collection removes.
- **X5 The AUR package `steno-bin`** (`feat/aur-package`). A PKGBUILD in the
  repository (`packaging/aur/`) installs the release `.deb`, verifies it against
  its `.asc` with `validpgpkeys` set to the release key
  (`048B527950E4F609B90E63495F8810A6E6D4DB46`), depends on `webkit2gtk-4.1`,
  `gtk3`, `libayatana-appindicator` and `pipewire`, and ships the `ufw` profile
  and the X4 marker. `publish` pushes the bump to the AUR on stable tags with a
  new secret, `AUR_SSH_PRIVATE_KEY`, which **Nicolai** creates with his AUR
  account; until then he pushes by hand.
- **X6 Nix on Linux** (`feat/nix-linux`). `flake.nix` gains
  `packages.x86_64-linux.steno`, built from the release `.deb` unchanged with
  `autoPatchelfHook` and `wrapGAppsHook3`, and `nixosModules.default`
  (`programs.steno.enable`): the package, the firewall ports (X3), a Secret
  Service provider (`services.gnome.gnome-keyring.enable`, which the module
  turns on unless one is set) and an assertion that PipeWire is on. The macOS
  output stays as it is (Homebrew and Nix).

## Release mechanics

### Tags and versions

A stable release is a merge commit that sets `[workspace.package] version` to
`X.Y.Z`, then a pushed `vX.Y.Z` tag. A candidate is the same with
`X.Y.Z-rc.N`. A tag is pushed only once Rust CI is green on its bump commit on
all three platforms. The `plan` job requires the tag to be `v<version>`; each tag
sits on its own version-bump commit, so no two tags share a commit count.

The build number is `git rev-list --count HEAD`. The `plan` and macOS jobs check
out with `fetch-depth: 0`. The macOS build gets the number through the
configuration merge (`{"bundle":{"macOS":{"bundleVersion":"<n>"}}}`), which
reaches both the Build and the Bundle step through `$RUNNER_TEMP/tauri-configs`.
Before any bundle is built, `plan` fails if the number is not above the highest
`sparkle:version` on the `appcast` branch.

### The workflow after S7

- **Trigger and names.** The trigger becomes `tags: ['v*']`, and the
  `desktop-v*` trigger goes. The release title becomes "Steno <version>". The
  manifest base URL passed to `updater-manifest.sh`, and the tag in the
  summary's notes, become `releases/download/v<version>`.
- **macOS bundle job.**
  - `check-bundle.sh --signed --handoff <build>` fails unless
    `CFBundleIdentifier` is `com.nicolaischmid.steno`, `SUPublicEDKey` is the
    Swift key, `CFBundleVersion` equals `<build>`, the designated requirement
    names team `KQB68F43PW`, and `codesign --verify --deep --strict` passes.
  - On tag runs only, after "Notarise the disk image", a "Handoff item" step
    signs. It runs `generate_appcast` from the Sparkle 2.10.0 tarball (SHA-256
    pinned) with `--ed-key-file -` (reading `SPARKLE_PRIVATE_KEY`, in this
    step's environment only), `--download-url-prefix
    .../releases/download/v<version>/`, `--maximum-deltas 0` and D8's rollout
    interval; fails unless the output carries `sparkle:edSignature`; and
    dry-runs `merge-appcast.py` against the current `appcast` branch.
  - The item goes into its own artifact, `sparkle-item`, outside the
    `steno-desktop-*` pattern that `assets` downloads.
  - Check secrets gains `SPARKLE_PRIVATE_KEY`, and the desktop README's secrets
    table lists it.
- **`publish`.**
  - A version with a hyphen publishes as today: `--prerelease --latest=false`.
  - A version without one: `gh release edit "$TAG" --draft=false
    --prerelease=false --latest`; upload the `appcast` branch's `appcast.xml`
    (`git fetch origin appcast && git show FETCH_HEAD:appcast.xml`) as the
    release's `appcast.xml`; bump the cask with
    `apps/macos/scripts/bump-homebrew-cask.sh` once `HOMEBREW_TAP_TOKEN` exists
    (D7); push the AUR bump once `AUR_SSH_PRIVATE_KEY` exists (X5); write the
    Nix flake lines (the macOS DMG's and the `.deb`'s hashes) to the summary;
    output whether the branch already has an item without a channel.
  - The lane releases stay pre-releases with `--latest=false`.
- **`handoff`.** Stable tags only, behind the GitHub environment `appcast`,
  whose required reviewer is Nicolai. Its job-level `if` reads `publish`'s
  output (with `plan` in `needs`), so it is skipped without asking once the
  branch has its item. After the approval it:
  - sets the item's `<pubDate>` to the approval time, in the form
    `generate_appcast` writes (`Tue, 13 Oct 2026 09:00:00 +0000`, Python's
    `email.utils.format_datetime` in UTC); Sparkle reads any other form as no
    date and offers the item to every group at once;
  - runs `apps/macos/scripts/publish-appcast.sh "$TAG" false`, with
    `STENO_RELEASE_APPCAST` pointing at the `appcast.xml` inside `sparkle-item`;
  - uploads the branch's new `appcast.xml` to the release with `--clobber`.

  Nicolai approves after R7. For a later release that fixes the handoff itself,
  the repository variable `HANDOFF_ITEM_REPLACE` is set to that version before
  its tag is pushed, and the job-level `if` also lets the job run then.
- **`release-notes.sh`.**
  - The opening sentence loses "this macOS build is a preview" and the pointer
    to the latest release. The `SHA256SUMS` line names every file but the
    OpenPGP signatures and `appcast.xml`.
  - Every `0.11.0-rc.N` and `0.11.0` carry the desktop-id paragraph (D5).
  - `0.11.0` carries the handoff paragraph: Swift users receive this release as
    an update; macOS asks once more for microphone, system audio and calendar,
    and once for each stored secret; an old "Steno" entry in Login Items may
    need removing (S6); phones stay paired, or pair again where D5 says so; the
    optional mixdown is now WAV; Whisper, Ultra and DE now transcribe with
    Parakeet v3; Homebrew users whose app updated itself can run
    `brew upgrade --greedy --cask nicolaischmid/tap/steno`; the install lines
    for Omarchy and NixOS; the Linux known issues.
  - The Windows and OpenPGP paragraphs stay.

### Lanes and "latest"

`desktop-stable` and `desktop-beta` keep their names and rules
(`updater-lanes.sh`). The first stable tag creates `desktop-stable` and moves
`desktop-beta` to 0.11.0. A stable tag is the only thing that sets `--latest`.

### `release.yml`

S7 deletes it before the first `v0.11.0*` tag, which also closes the concurrency
item. The Swift scripts the desktop workflow still calls stay in
`apps/macos/scripts/` until S9, where `ReleaseScriptsTests` and
`AppcastScriptsTests` keep testing them.

If the rehearsal shows that a Swift build cannot take the handoff, or that the
Swift Login Items entry must be unregistered by the Swift app itself (S6), a
dedicated PR restores `release.yml` with a `swift-v*` trigger for a bridge
release. It must ship before the handoff item exists, because the item must
carry the highest `sparkle:version` on the appcast; Swift installs that skip it
(offline, or on `v0.9.0-rc.1`'s feed) still take the handoff directly.

### Homebrew, AUR and Nix

The first stable cask bump is one tap commit by **Nicolai**, made in G3 and
before `HOMEBREW_TAP_TOKEN` is set. It sets `version` and `sha256` for 0.11.0,
and also: `url ".../releases/download/v#{version}/Steno_#{version}_aarch64.dmg"`;
`livecheck` for stable versions only (`/^v?(\d+(?:\.\d+)+)$/`); `zap` adds
`~/Library/Application Support/com.nicolaischmid.steno`,
`~/Library/WebKit/com.nicolaischmid.steno`,
`~/Library/Caches/com.nicolaischmid.steno` and
`~/Library/Preferences/com.nicolaischmid.steno.plist`, keeping the `uno.*`
entries; the caveat says Steno updates itself. `auto_updates true` stays.

The AUR package is X5. In S7, `flake.nix`'s macOS output gets the new URL, checks
for `Contents/MacOS/steno-desktop` and `bundleId = "com.nicolaischmid.steno"`,
and its UPDATES text names Settings' automatic-check switch; X6 adds the Linux
outputs. The flake bump stays a manual PR made from the job summary.

## The Sparkle handoff

### The handoff item

One item without a channel, served from the `appcast` branch (for
`v0.9.0-rc.2` and later) and as the `appcast.xml` asset of every stable release
(for `v0.9.0-rc.1`, through `releases/latest`). `sparkle:version` is the commit
count (above 542), `sparkle:shortVersionString` `0.11.0`,
`minimumSystemVersion` 15.0 and `hardwareRequirements` `arm64`; the enclosure is
`.../releases/download/v0.11.0/Steno_0.11.0_aarch64.dmg` with its length,
`sparkle:edSignature` and the approval's `pubDate`. The Swift items stay below
it. Nix installs see it but cannot install from the read-only store.

### What Sparkle checks

| Check | How the Tauri bundle passes |
|---|---|
| The archive's EdDSA signature verifies against the running app's key | Signed with the same `SPARKLE_PRIVATE_KEY` |
| The new bundle keeps a public key | S6 puts the Swift `SUPublicEDKey` in `Info.plist`; `--handoff` asserts it |
| The new bundle's code signature is valid (it need not match the old requirement, since the EdDSA check passed) | Developer ID, team `KQB68F43PW`, hardened runtime, sidecar signed inside, notarised |
| The archive holds `Steno.app` (found by name; its id is not compared) | `productName` "Steno" |
| The build number is above the host's | The commit count, checked in `plan` and by `--handoff` |
| Minimum system version and architecture | 15.0 and `arm64` |

Sparkle mounts the DMG, swaps the bundle at the host's path (usually
`/Applications/Steno.app`), deletes the old one and relaunches it. From then on
that path holds `com.nicolaischmid.steno`.

### First launch after the handoff

| What | What happens | Proven by |
|---|---|---|
| Meetings, audio, models | The same support directory, same schema | R3 |
| Preferences | `swift_import` reads `uno.schmid.steno.mac` explicitly (S6) | R3 |
| TCC grants | None carry over; onboarding opens on its permissions page (S3), and the user grants once | R3: one prompt each, then none |
| API key | Read once on the import step, with one keychain prompt, and written again as the new app's item | R3: one prompt, then summaries run with none |
| Handover identity | Exported once, with a prompt that may ask for the login password, and stored as the new app's PEM entry; the Swift item stays | R3: the phone uploads without pairing again |
| Codex sign-in | The Codex CLI's own `auth.json`, untouched | R3 |
| Login item | The new app registers itself when `launch_at_login` is on; the Swift entry is handled as S6 says | R3: Steno starts once at login |
| Updates | The Tauri updater reads `desktop-stable` daily (S4) | R4 |

## Landing page and README

S8 is merged after R8 passes.

- **`apps/site/src/lib/site.ts`.** `download` stays `releases/latest`.

  | Platform | Note | Listed as released |
  |---|---|---|
  | `mac` | "Apple Silicon · notarised" | yes, as today |
  | `linux` | "GNOME (.deb · AppImage) · Omarchy (AUR) · NixOS (flake)", naming only the targets whose gates passed | once at least one Linux gate passes |
  | `win` | "x64 · installer not code-signed" | once its gate passes |

  The download button needs no change. The closing CTA's Linux tile shows the
  install line per target (the `.deb`, `yay -S steno-bin`, the flake).
- **`how-its-built.tsx`.** The subhead loses "is moving". The closing paragraph:
  "Every desktop runs the same Rust app. On the Mac it replaced the original
  Swift app as an ordinary update, with your meetings, settings and paired phone
  where you left them."
- **`open-source.tsx`.** "Swift Mac app · Rust core · Tauri shell" becomes "Rust
  core · Tauri shell".
- **`page.tsx`.** The JSON-LD `operatingSystem` lists the released platforms.
- **README.**
  - The status banner says stable and links the release. The release badge
    drops `include_prereleases`, Rust CI replaces Swift CI, and the platform
    badge names the released desktops.
  - Install has one block per OS. **macOS:** Homebrew, the DMG and Nix; updates
    come through the app. **Windows:** the MSI or the `-setup.exe`, not
    code-signed, so SmartScreen asks first; the hash against `SHA256SUMS`.
    **Linux:** GNOME (the `.deb` or the AppImage, verified with the `gpg
    --verify` and `sha256sum --check` lines from the desktop README, whose key
    URL moves to `v<version>`); Omarchy (`yay -S steno-bin`, `ufw allow Steno`,
    the tray behind waybar's expander, the Hyprland window rules if X2 needs
    them); NixOS (the flake input and `programs.steno.enable = true`).
  - "Homebrew details" says the tap follows stable releases (D7). "For
    developers" puts the Rust workspace first; the Swift lines go in S9.
- **`apps/desktop/README.md`.** Release, "Cutting a release", "When a run fails",
  "Publishing, on a tag" and the Layout table use `v<version>` and the full
  release; "A bad release" covers the first stable release (Rollback); "Not here
  yet" loses what the packages close and marks the clip player as following.
- **Comments.** The "never latest" comments in `updater.rs` and at the top of
  `desktop-release.yml` go.

## Rehearsal

Nothing reaches Swift users until R1 to R6, the dogfood, the Linux gates for the
targets to be listed and the stability count have passed (G2). The stable tag
then rebuilds the same code with only the version and the build number changed.
R7 checks that build before Nicolai approves the handoff item, and R8 checks it
through the real feed while the phased rollout has reached only its first group.

The steps run in three places:

- **Forge** (`ssh forge`), which has no `sudo`. Work goes under
  `~/steno-handoff/`, never `~/steno-calibration/`, and no app goes into
  `/Applications`. `ditto` copies only into a path that does not exist; to
  replace a bundle, remove the old one first. Assets and artifacts are fetched
  on atlas and copied over. Before R2 and before R7, `defaults export
  uno.schmid.steno.mac ~/steno-handoff/defaults-before.plist`; after each,
  `defaults import` restores it and
  `~/Library/Caches/uno.schmid.steno.mac/org.sparkle-project.Sparkle` is
  removed, since `sparkle-cli` writes the bundle id's real domain.
- **A fresh account on Nicolai's Mac**, a new macOS user per step that says so;
  R4 and R6 reuse R3's. Every Steno build there goes into
  `~/Applications/Steno.app` and is opened by path; nothing touches
  `/Applications`. A modal dialog that the step does not list fails it; the
  "Background Items Added" notification does not.
- **Nicolai's Linux machines** for the Linux gates.

**The local feed** (R2, R3, the dogfood) holds the candidate's `sparkle-item`
and DMG, with the enclosure rewritten to `http://localhost:8765/<dmg>` (the
EdDSA signature covers the file, and the Swift app does not set
`SURequireSignedFeed`; ATS refuses bare IP addresses). It is served with
`python3 -m http.server 8765 --bind 127.0.0.1` in the account that runs the
step, and `curl -fsS http://localhost:8765/appcast.xml` must answer first. The
server's access log (`GET /appcast.xml`, and `GET /<dmg>` for an install) proves
the feed was read.

**`sparkle-cli`** runs the same validator and installer as the in-app updater;
Sparkle 2.9.0 removed it from the binary download. Build it once on Forge from
the `2.10.0` tag (ad-hoc signing is the project default):

```sh
nice -n 19 xcodebuild -project Sparkle.xcodeproj -scheme sparkle-cli \
  -configuration Release -derivedDataPath ~/steno-handoff/build
ditto ~/steno-handoff/build/Build/Products/Release/sparkle.app \
  ~/steno-handoff/tools/sparkle.app
```

Each case is a Launch Agent started with `launchctl bootstrap gui/$(id -u)
<plist>` (unique `Label`; absolute `ProgramArguments` with the case's flags and
`--user-agent-name steno-rehearsal`; `RunAtLoad` true, no `KeepAlive`;
`StandardOutPath` and `StandardErrorPath` under `~/steno-handoff/logs/`). Poll
`launchctl print gui/$(id -u)/<label>` until `state = not running`, at most ten
minutes, read `last exit code`, then `bootout`. Cases run one at a time: the next
starts only when `launchctl list | grep uno.schmid.steno.mac-sparkle` is empty.
Swift release candidates read `beta`, so the runs pass `--channels beta`, except
case e.

### The steps

- **R1 Desktop-id conversion** (**Nicolai**, a fresh account on the Mac, and
  his GNOME machine).
  1. Install `desktop-v0.1.0-rc.2` (on Linux, the AppImage). Grant the
     permissions, set an API key, turn on launch at login, move a floating
     panel, and record one short call.
  2. When the candidate is on `desktop-beta`, check for updates and install.
  3. Download the speech models in Settings, run Process again on the first
     call, and record a second call.

  Pass when the app relaunches as `com.nicolaischmid.steno`; launch at login
  starts it once; the panel opens where it was moved (the anchor read from the
  old directory); on the Mac the TCC prompts appear once each at the second
  call and one keychain prompt per item the desktop-id build created
  (`handover-identity` and the API key, both at launch), then none; both calls
  are listed, transcribed and summarised.
- **R2 Handoff mechanics** (Forge; the cutover plan's test 6).
  1. Fetch the candidate's `sparkle-item` on atlas
     (`gh run download <run> -n sparkle-item`) and copy it over.
  2. Each case starts from a fresh `ditto` copy of its Swift app under
     `~/steno-handoff/apps/<case>/` and runs
     `sparkle --feed-url <feed> --check-immediately [--channels beta] --verbose <app>`.
     The server's access log shows every case fetching the feed.

  | Case | Swift build | Feed | Pass |
  |---|---|---|---|
  | a | `v0.10.0-rc.2` (542) | the local feed | Exit 0; the checks below |
  | b | `v0.10.0-rc.2` | the local feed with a damaged `edSignature` | Exit 1; the log names the EdDSA validation; the app is unchanged |
  | c | `v0.10.0-rc.2` | the local feed with `sparkle:version` 500 | Exit 4, "No new update available" |
  | d | `v0.9.0-rc.4` (450) | the `appcast` branch's feed plus the candidate item | Exit 0; the candidate is installed |
  | e | `v0.9.0-rc.1` (244), no `--channels` | the same feed | Exit 0; the item has no channel and suits build 244 (the real route is R8's) |

  Case a's checks: the bundle at the Swift app's path now has
  `CFBundleIdentifier` `com.nicolaischmid.steno`, `CFBundleExecutable`
  `steno-desktop`, the candidate's build number and the Swift `SUPublicEDKey`;
  `codesign --verify --deep --strict` passes; `codesign -d -r-` names team
  `KQB68F43PW`; `spctl -a -t exec -vv` says "Notarized Developer ID". This is
  the proof, on the real bundles, that Sparkle hands over across bundle ids.
- **R3 The real handoff** (**Nicolai**, a fresh account; the cutover plan's
  tests 1 to 3). The phone keeps one pairing, so pairing it here unpairs his
  daily install until he pairs it there again.
  1. Install `v0.10.0-rc.2`. Grant every permission, set an API key, pair the
     phone, record and process a meeting, and turn on launch at login. Note what
     System Settings > General > Login Items shows.
  2. Quit. Run `defaults write uno.schmid.steno.mac SUFeedURL
     http://localhost:8765/appcast.xml` (Sparkle 2.10 honours it in release
     builds), and relaunch.
  3. Check for Updates and install.
  4. Pass when:
     - the app comes back as `com.nicolaischmid.steno`, and the meeting is
       listed;
     - onboarding opens on its permissions page, and each of microphone, system
       audio and calendar prompts once and never again;
     - the import step shows its note, then exactly one keychain prompt for the
       API key and one for the handover key (which may ask for the login
       password); record each prompt's wording;
     - after them, a summary runs with the stored key and no prompt;
     - the phone uploads without pairing again;
     - a call records both lanes;
     - after logging out and back in, Steno starts once. Record what Login Items
       shows, old entry included, and what the old entry does; S6's handling
       follows from it;
     - Settings shows the CoreML Parakeet as installed.

     Any other modal dialog fails R3.
  5. Run `defaults delete uno.schmid.steno.mac SUFeedURL`.
- **R4 Updates after the handoff** (**Nicolai**, R3's account; the cutover
  plan's test 4).
  1. Publish the next candidate, which changes only the version.
  2. Quit. Set S4's stored last check time to 25 hours ago in the file the S4
     PR names, and relaunch.

  Pass when the scheduled check finds the new candidate and offers or installs
  it; after the relaunch, `ps -o command= -p $(pgrep -u "$(id -u)" -x
  steno-desktop)` shows `~/Applications/Steno.app/Contents/MacOS/steno-desktop`
  and About shows the new version; no keychain or TCC prompt appears again; with
  the speech setting on the ONNX engine for one meeting, `steno-speech-sidecar`
  starts from the same bundle (switch it back).
- **R5 Fresh install** (**Nicolai**, a fresh account; the soak on Forge; the
  cutover plan's test 5).
  1. With networking off, open the downloaded, quarantined DMG;
     `spctl -a -t open --context context:primary-signature -vv` accepts it, and
     the app opens. Turn networking back on.
  2. Onboarding asks for each permission.
  3. Settings downloads the CoreML Parakeet and the diarizer models, with
     progress.
  4. A five-minute call is transcribed, diarized and exported.
  5. On Forge, with `HOME=~/steno-handoff/soak-home` (the models copied into its
     `Library/Application Support/Steno/Models/`, no API key, no destination)
     and the candidate's `steno` built from its tag, under `/usr/bin/time -l`:
     `steno process mic.wav --system-lane system.wav --source mac-call --engine
     parakeet-v3` on a two-hour two-lane FLEURS recording (generated, never
     committed); and `steno dev bakeoff` on a two-hour mono 44.1 kHz m4a and a
     two-hour two-channel 48 kHz Float32 CAF without sidecars, each in its own
     directory. Pass: every peak resident memory stays under A1's bound, and
     the transcripts are complete.
- **R6 Rollback drill** (**Nicolai**, R3's account).
  1. Save the Tauri app's requirement without its prefix
     (`codesign -d -r- ~/Applications/Steno.app 2>/dev/null | sed -n 's/^designated => //p' > tauri-dr.txt`)
     and keep a copy of the app (`ditto ~/Applications/Steno.app ~/steno-r6/Steno.app`).
  2. Quit Steno, remove `~/Applications/Steno.app`, and `ditto` the
     `v0.10.0-rc.2` app from its DMG into its place.
  3. Pass when:
     - it opens the v5 database (P2) without an error and lists every meeting,
       including the ones the Tauri app recorded;
     - it records with its own TCC grants and reads its own keychain items with
       no prompt;
     - the phone uploads one recording to it;
     - Login Items shows what R3 recorded, and Steno starts once at login.
  4. Quit, remove the bundle, and `ditto ~/steno-r6/Steno.app` back. Pass when
     `codesign --verify -R="=$(cat tauri-dr.txt)"` passes on it, it lists the
     meeting the Swift app received in step 3, the phone uploads again without
     pairing, and a retried upload of that recording is answered as P2 says,
     with no second meeting and nothing lost.
- **Dogfood** (**Nicolai**, his own account). After R1 to R6 pass:
  1. Re-pair the phone with his daily install.
  2. Check whether his account holds a desktop-id identity
     (`security find-generic-password -s uno.schmid.steno.mac -a handover-identity`)
     and whether `~/Library/Application Support/Steno/preferences.json` holds
     `steno.swiftImportRan`. If the latter does, the dogfood does not test the
     import, and R3 is its only proof. No desktop-id install in this account is
     updated before the dogfood.
  3. His daily install takes the candidate through the local feed, as in R3, and
     the `SUFeedURL` default is deleted afterwards.

  Pass: the meetings count towards the stability gate (G2); one phone upload
  without pairing again; no regression he would not ship.
- **R7 The stable build, before approval** (Forge). Fetch `v0.11.0`'s
  `sparkle-item` on atlas: no `sparkle:channel`,
  `sparkle:phasedRolloutInterval` 86400, the enclosure is the `v0.11.0` asset
  URL. Run R2's cases a and e against the public DMG, serving the item locally
  without rewriting the enclosure.
- **R8 The stable build, through the real feed** (**Nicolai** and atlas). Five
  minutes after the `handoff` job's push, once both the raw `appcast.xml` and
  `releases/latest/download/appcast.xml` show the 0.11.0 item:
  - in two fresh accounts, `v0.10.0-rc.2` and `v0.9.0-rc.1` each record one
    meeting, then take 0.11.0 through Check for Updates with no `SUFeedURL`
    default. Pass: each relaunches as `com.nicolaischmid.steno` 0.11.0, lists
    the meeting, and shows only R3's prompts;
  - a 0.11.0 install's Check for Updates reports up to date against
    `desktop-stable` (HTTP 200);
  - `gh api repos/NicolaiSchmid/steno/releases/latest --jq .tag_name` answers
    `v0.11.0`;
  - the branch's only item without a channel is 0.11.0, with the rollout
    interval and the approval's `pubDate` in the Swift items' date form;
  - **Homebrew** (after Nicolai's tap commit, his own account): pull the tap
    (`git -C "$(brew --repository nicolaischmid/tap)" pull --ff-only`, or `brew
    tap nicolaischmid/tap`); `HOMEBREW_NO_AUTO_UPDATE=1 brew fetch --cask
    nicolaischmid/tap/steno` downloads 0.11.0 and verifies it; the DMG in
    `brew --cache --cask nicolaischmid/tap/steno` holds
    `Contents/MacOS/steno-desktop`;
  - **AUR and Nix:** on the Omarchy and NixOS machines, the Linux gates' install
    steps run against `v0.11.0`.

### The Linux gates

Each runs on Nicolai's own machine, on the last candidate and again on
`v0.11.0`, before the site lists the target. Every step's recording counts
towards the stability gate.

- **GNOME** (Ubuntu or Debian on hardware, Wayland session).
  1. Install the `.deb` (`sudo apt install ./steno-desktop_<v>_amd64.deb`),
     verified first with the README's `gpg --verify` and `sha256sum --check`.
  2. Onboarding asks for the microphone; Settings downloads the speech model
     with progress; Settings > Recording lists the input devices (A7).
  3. Pair the phone; it uploads a recording.
  4. Join a call in a browser: the detection prompt appears (A8); record it,
     with the floating panel on top; stop; it is transcribed, diarized and
     exported.
  5. Start a recording, then log out: the recording is saved (P5). Start one,
     then reboot: saved. Start one, then `kill -9` the app: at the next launch
     it is recovered and processes (P3).
  6. The AppImage starts and finds the same meetings.
- **Omarchy** (Arch with Hyprland on Wayland).
  1. `yay -S steno-bin` installs the package; the PKGBUILD verified the `.deb`'s
     signature. `sudo ufw allow Steno`.
  2. Start Steno from the Omarchy menu: the tray icon shows in waybar (behind
     the expander); the floating panels float, stay on top and keep their place
     (X2).
  3. Secrets: set an API key, pair the phone, restart Hyprland (log out and
     in), and check that the key and the pairing are still there and that
     another app's secret in the keyring (for example a browser's) still opens
     (P7).
  4. The phone uploads a recording through the firewall (X3).
  5. Steps 4 and 5 of the GNOME gate, with the logout through Omarchy's menu
     (X1).
  6. Settings says updates come from the package manager (X4); after
     `yay -Syu` to the next candidate, the meetings and the pairing are kept.
- **NixOS** (x86_64-linux, the desktop Nicolai runs there).
  1. Add the flake input, set `programs.steno.enable = true`, and
     `nixos-rebuild switch`.
  2. Steps 2 to 5 of the GNOME gate, with the phone through the firewall the
     module opened.
  3. Launch at login survives a `nix-collect-garbage -d` after an update (X4).
  4. Settings says updates come from the package manager.

**The Windows gate** (before the site lists Windows): a Windows machine runs
the `--ignored` WASAPI tests, one real call, a logoff during a recording that
saves it (P5), and a `kill` that P3 recovers.

## Order of operations and gates

1. **Nicolai confirms** the choices marked "to confirm" (D5's directories and
   iOS availability, D9's final choices, D12).
2. **Every package is written in parallel:** S1 to S7, A1 to A9, P1 to P8, X1 to
   X6.
3. **Gate G1.** All of them are merged, and every row of the blocking list is
   closed. The audio path is final (D9).
4. **Bump to `0.11.0-rc.1` and tag it.** This publishes a pre-release and moves
   `desktop-beta`; no handoff item is signed into the appcast. Desktop-id
   installs convert.
5. **Gate G2.**
   - R1 to R3 pass on candidate N. R4 publishes N+1, which changes only the
     version, and R5, R6 and the dogfood pass on N+1, in that order.
   - The Linux gate of each target to be listed passes.
   - **The stability count** (D12): R5's soak passes, and ten real meetings on
     the Mac and five on each Linux target to be listed are recorded, processed
     and exported without a failure. Each count includes a meeting over an hour,
     a phone upload and a device switch during a call. A failure is a lost or
     truncated recording or lane, a failed job that Process again does not fix,
     or a crash.
   - A fix becomes a new candidate. The steps it touches run again, and a fix
     to the audio path restarts the stability count.
6. **Before the stable tag, Nicolai** creates the `appcast` environment with
   himself as required reviewer, "Prevent self-review" off and deployment tags
   `v*` (`gh api repos/NicolaiSchmid/steno/environments/appcast --jq
   '[.protection_rules[].type]'` includes `required_reviewers` and
   `branch_policy`); turns off Mac availability for the iOS app in App Store
   Connect (D5); and, for X5, creates `AUR_SSH_PRIVATE_KEY` or plans the first
   AUR push by hand.
7. **Bump to `0.11.0` on a fresh commit and tag it.** This publishes the full
   release as "latest", creates `desktop-stable`, moves `desktop-beta` to
   0.11.0, uploads `appcast.xml`, and puts the flake lines in the summary. The
   `handoff` job waits for approval. Nicolai opens the flake bump PR.
8. **Gate G3.** R7 passes. Then Nicolai approves the `handoff` job, makes the
   first cask bump by hand (D7) and the first AUR push if not automated, and R8
   passes. Merge S8 and the flake bump; Vercel deploys the site. Nicolai sets
   `HOMEBREW_TAP_TOKEN`.
9. **Rollback window: 14 days.** No database migration merges. Watch the
   issues.
10. **S9**, the Swift removal.

## Rollback

**Before step 7**, nothing reaches Swift users or stable installs. A bad
candidate is handled as the desktop README's "A bad release" says for
`desktop-beta`.

**If R7 fails,** Nicolai rejects the `handoff` job and runs
`gh release edit v0.11.0 --prerelease`. No Swift build has seen the item and the
cask is not bumped. Candidate installs take 0.11.0 through `desktop-beta`, and a
fresh download in that window reads `desktop-stable`; if the failure is in the
bundle, put both lanes back as "A bad release" says. The fix ships as `0.11.1`.

**A bad handoff item, while it rolls out:**

1. Revert the item's commit on the `appcast` branch, then
   `gh release upload v0.11.0 appcast.xml --clobber` with the reverted file, and
   allow five minutes for raw.githubusercontent's cache. Installs that have not
   downloaded the item stop seeing it; installs that downloaded it with
   automatic download on install it when they quit; the phased rollout keeps
   that group small.
2. If fresh installs are affected too, `gh release edit v0.11.0 --prerelease`
   takes the release out of "latest" (`releases/latest` then answers 404, which
   also cuts off build 244's feed). In the same hour, take the platform off the
   site's released list.
3. Revert the flake bump; Nicolai reverts the tap commit and the AUR bump.

**Users who already took a bad release** cannot go back through Sparkle. Fix
forward with `0.11.1`, which the daily check (S4, R4) delivers. Stop the spread
on the lanes as "A bad release" says; the first stable release has no earlier
release on `desktop-stable`, so there `latest.json` is deleted
(`gh release delete-asset desktop-stable latest.json`); S8 adds this to that
README section.

**As a last resort for one user:** quit Steno, then drag the `v0.10.0-rc.2` app
from its DMG onto the Tauri app in Finder and choose Replace. It has its own TCC
grants and keychain items still, and opens the v5 database (R6). Then either
revert the item for everyone before relaunching it, or, for this user alone, run
`defaults write uno.schmid.steno.mac SUAutomaticallyUpdate -bool false` while it
is still quit, open it, and choose Skip This Version when 0.11.0 is offered.

**The database.** Every migration until S9 is mirrored in `Migrations.swift`;
v5 (P2) lands before the first candidate, and none merges in the window. GRDB in
the last Swift release ignores an applied migration it does not know, so going
back stays possible until the Swift app is removed.

## Changes to other plans in this plan's PR

- **`.plans/2026-10-04-mac-cutover.md`.** A note at the top says that this plan
  extends it, where it changes it, and which details no longer hold.
- **`.plans/2026-10-02-rust-core-and-tauri-shell.md`.**
  - The status line, the WP9b progress row and the owner sentence of "Open
    after the port" point here.
  - These places now point at this plan: WP9's opening line and its cutover
    paragraph; the "Updates" and "Pending speaker reviews" lines under "Beyond
    the bridge"; seam (4); the first **WP9b.** item; the Shell section's
    login-item line (D4); the **WP9b.** item on the other Swift fixes (D9; S7
    deletes it).
  - The closed **WP9b.** item about the first `desktop-v*` tag is deleted. The
    other items stay until the pull requests that fix them delete them.
