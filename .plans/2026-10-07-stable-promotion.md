# Stable promotion: the Tauri app becomes the Steno release

Status: planned 2026-10-07, not started; waits for Nicolai to confirm the
decisions below. Nicolai decided on 2026-10-07 to promote the Tauri app from
beta to stable: release tags and the build pipeline produce the Tauri app, the
landing page offers it, and existing users of the Swift Mac app end up on it
without losing meetings, settings, secrets, paired phones or the login item.

This plan extends `.plans/2026-10-04-mac-cutover.md` and supersedes parts of it.
It replaces the cutover plan's opening gate (every unticked parity line) with
the blocking list below, replaces its step 2 (the Sparkle handoff) and step 3
(distribution), and proposes answers to the open choices in its steps 1 and 5.
The cutover plan's inventory table, steps 4, 6 and 7, its risks and its tests
still apply; the rehearsal below runs those tests. In
`.plans/2026-10-02-rust-core-and-tauri-shell.md`, this plan owns every
**WP9b.** item in "Open after the port".

## Facts this plan rests on

Checked on 2026-10-07 against `main` (`e13ab9b8`), the releases and the
`appcast` branch.

- **Every release so far is a pre-release.** Swift: `v0.9.0-rc.1` to
  `v0.10.0-rc.2`. Tauri: `desktop-v0.1.0-rc.1` and `desktop-v0.1.0-rc.2`, plus
  the rolling `desktop-beta` lane. `GET /releases/latest` answers 404, so the
  site's Download button (`site.download` in `apps/site/src/lib/site.ts`) and
  the README's "latest release" link lead nowhere.
- **Every installed Swift build is a release candidate, so all of them read
  Sparkle's `beta` channel.** `UpdateChannels.allowed` gives `["beta"]` to any
  version with a hyphen (`apps/macos/Steno/Services/UpdateChannels.swift`), and
  every item on the `appcast` branch carries `<sparkle:channel>beta`. Because
  of this, the cutover plan's idea of publishing the handoff item on the beta
  channel first would not hold anyone back: every Swift user would get it at
  once. A test feed has to do the staging instead (Rehearsal).
- **The newest Swift build number is 542** (`sparkle:version` of
  `v0.10.0-rc.2`). Sparkle compares build numbers only. The Tauri bundle's
  `CFBundleVersion` is its marketing version today ("0.1.0-rc.2"), and Sparkle
  would rank that below 542. `main` has 2068 commits, so the commit count,
  which is the Swift scheme, is far above 542. Tauri 2.10's bundler takes
  `bundle.macOS.bundleVersion`.
- **The two desktop pre-releases published without manual steps** (runs
  37349614159 and 37425455820): the `publish` job and an MSI for an `-rc.N`
  version both worked. The **WP9b.** item about the first `desktop-v*` tag is
  closed, and this PR deletes it.
- **The phone finds the computer by `macID`, not by name.** `macID` is derived
  from the handover certificate's fingerprint (`HandoverIdentity::mac_id`), and
  the phone looks the service up by it (`findByMacID` in
  `mobile/src/features/sync/use-upload-coordinator.ts`). A new identity
  therefore breaks every pairing, not only the pinning check, so importing the
  Swift identity matters.
- **Asset names differ.** The Swift release is `Steno-<v>.dmg`. The Tauri
  release has `Steno_<v>_aarch64.dmg`, `Steno_<v>_aarch64.app.tar.gz`,
  `Steno_<v>_x64_en-US.msi`, `Steno_<v>_x64-setup.exe`,
  `steno-desktop_<v>_amd64.deb` and `.AppImage`. The Homebrew cask (still at
  `0.9.0-rc.1`, since `HOMEBREW_TAP_TOKEN` was never set) and `flake.nix` both
  build the Swift URL.

## Decisions to confirm

Each decision has a recommendation; the rest of this plan assumes it.

- **D1 Tag scheme: `v*` for the Tauri app.** Recommended. There is one
  product, so one tag line. The Homebrew cask's URL and livecheck and
  `flake.nix` already expect `v<version>`, and GitHub's release list reads as
  one history. This needs `release.yml` gone before the first `v0.11.0*` tag,
  or that tag would also start a Swift build (S6). The updater lanes keep
  their names `desktop-stable` and `desktop-beta` for good, because the
  installed desktop builds have those URLs compiled in (`updater.rs`). The old
  `desktop-v*` tags and releases stay.
  Alternative: keep `desktop-v*`. That works but splits the history, and the
  cask and flake would need a second URL scheme.
- **D2 Version: the line continues at 0.11.0.** Candidates are
  `0.11.0-rc.N` and the first stable release is `0.11.0`. This is above every
  Swift tag, so no updater sees an older version. `1.0.0` remains the name for
  the day the v1 checklist (issue #74, re-scoped to the Rust app) is done.
  Alternative: make the first stable `1.0.0`. The mechanics do not change.
- **D3 The parity gate shrinks to the blocking list below.** The cutover plan
  waited for every unticked parity line. This plan blocks only on what loses
  data, breaks a core flow (record, transcribe, name, pair, update) or fails
  silently. Convenience items follow the stable release (What follows).
- **D4 Login item: `SMAppService.mainApp` on macOS, as in Swift.**
  Recommended. The shell's macOS `LoginItem` calls `SMAppService` through
  `objc2` instead of `tauri-plugin-autostart`'s Launch Agent. The Swift
  registration names the bundle (identifier and team), so after the handoff it
  keeps launching the new app. There is nothing to migrate and there is no
  second item, and the `requiresApproval` copy stays reachable. Linux and
  Windows keep the plugin. If the binding needs `unsafe`, it goes in one safe
  wrapper module with a comment on each invariant, and that PR adds the module
  to the `unsafe` rule in `AGENTS.md`.
  Alternative: the cutover plan's step 5 (keep the Launch Agent, unregister
  `SMAppService` on first launch). That keeps more code and the two-item risk.
- **D5 Installs of the `uno.schmid.steno.desktop` build are converted through
  their lane.** The first `0.11.0-rc.1` carries the Mac bundle id. Desktop-id
  Mac installs read `desktop-beta` and take it as an ordinary update. Each
  such install then asks once for microphone, system audio and calendar (TCC
  grants follow the bundle id), and once per keychain item it created itself.
  The release notes say so. The other way, new lane names, would strand those
  installs as a second app on the same database.
- **D6 Linux and Windows.** Every stable tag publishes all three platforms,
  as today. The site marks a platform as released only once its gate passes
  (Linux and Windows at the stable tag). Recommended: Linux becomes released
  with the stable tag if its gate passes; Windows becomes released only once a
  Windows machine has run it.
- **D7 Homebrew: the cask follows stable releases only.** Nicolai creates
  `HOMEBREW_TAP_TOKEN` once, so the release workflow bumps the cask. Until the
  token exists, every bump is a manual commit to the tap. Release candidates
  are tested from the DMG, not through brew.
- **D8 Sparkle items after the handoff: every stable release adds its own
  item,** without a channel and with Sparkle's phased rollout
  (`--phased-rollout-interval 86400`: seven groups, one day apart; a manual
  Check for Updates skips the wait). This continues until the Swift app's
  removal (S8) plus three months; after that the `appcast` branch freezes on
  its last item. A Swift build that was offline for months then lands on a
  current Tauri release, not on the first one. Pre-releases never add an item,
  because every Swift install reads `beta`.

## What blocks stable on the Mac

Each row is a **WP9b.** item or an unticked parity line, with the package that
closes it (Work packages).

| Item | Blocks | Why | Package |
|---|---|---|---|
| The Rust app cannot download the CoreML Parakeet model | yes | A fresh Mac install cannot transcribe (cutover test 5) | S1 |
| Whisper, Ultra and DE have no Rust engine | yes | A Swift user who chose one gets Parakeet v3 in the sidecar, after an unannounced 2.6 GB download | S1 |
| Silent 2.6 GB download inside the pipeline | yes | Fails silently; also blocks the first Linux release | S1 |
| Diarizer Settings wording and model licence notices | yes | Wrong model and size shown; WeSpeaker's CC BY 4.0 licence requires attribution | S1 |
| Meeting detection and its prompt | yes | Swift users start most recordings from the prompt; without it meetings go unrecorded | S2a |
| Auto-stop after a call (90 s grace, Keep recording) | yes | Recordings run until stopped by hand | S2a |
| Calendar lookup at recording start | yes | Meeting titles and attendee names, which seed speaker naming, are lost | S2b |
| Permissions probe and the `unknown` rule; the host's real `Permissions` | yes | A fresh install shows every permission as granted (the fake) and can record silence | S2b |
| Update schedule (the host's `Updater`) | yes | Without daily checks there is no way to fix a release forward after the one-way handoff | S3 |
| QR encoder | yes | A new phone cannot pair | S3 |
| Bonjour re-publish after a network change; the computer name | yes | A laptop that changes network becomes invisible to the phone until Steno restarts | S4 |
| Bundle id, build number, `SUPublicEDKey`, first-launch import | yes | This is the handoff itself (cutover steps 1, 4, 5, 6) | S5 |
| Concurrency group across the two release workflows | yes, closed by S6 | `release.yml` is deleted, so only one signing workflow remains | S6 |
| First run of `publish` | no, closed | Both desktop pre-releases published without manual steps | this PR |

## What follows

These do not block the stable release. Each keeps or gets an owner line in
"Open after the port" of the Rust plan.

- The tray's badge for pending speaker reviews, the menu bar's queue and five
  recent meetings, and the macOS Record and Find Meetings menu items. The page
  already answers ⌘⇧R and ⌘F, and the main window shows pending reviews.
- The clip player, so that a speaker's sample plays. Speakers can still be
  named from their turns. It is the borderline item: if Nicolai wants it
  before the stable release, it joins S3.
- Importing the floating panel's anchor from Swift. The panel opens at its
  default place.
- The whole-lane decode (1.4 GB for a two-hour 48 kHz lane). It is listed
  under the first Linux release. It also affects the Mac, so R4 measures peak
  memory on a two-hour recording. If memory runs short on a 16 GB Mac, it
  becomes blocking.
- Removing the Swift app (cutover step 7) is S8, after the rollback window.
- The **Unowned.** items in the Rust plan stay unowned.

**Settled here** (the parity notes' "a plan decides"; Nicolai may overrule
any of them):

- The mixdown stays 16 kHz WAV. The release notes say that the optional
  mixdown is now WAV, not AAC.
- The 64-tap resampler stays as it is.
- The sidecar's 2 ms lag is accepted.
- The AAC priming offset of 23 to 48 ms on phone recordings is accepted; it
  is far below the length of a word.
- Call mode without an output client behaves the same in both apps, so it is
  not a regression.
- The Swift defects listed under "Store", "Adapters", "Handover", "LLM" and
  "Bridge" close with the handoff. Swift ships no further release (S6).

## Work packages

Each package is one pull request off `main`, unless it says otherwise. It is
written by an implementation agent and taken through the review pipeline in
`steno-pipeline-agent.md` (simplify, three reviews, fix, merge by merge
commit). Steps marked **Nicolai** need him: secrets, a physical Mac session
for TCC prompts, the phone. Packages S1 to S4 run in parallel. S5 and S6
merge one after the other, with no tag pushed in between.

- **S1 Speech models on the Mac** (`feat/rust-mac-speech-models`).
  - Download the CoreML Parakeet v3 model. The source is the Hugging Face
    repository FluidAudio reads (`FluidInference/parakeet-tdt-0.6b-v3-coreml`),
    pinned to a commit like `PARAKEET_V3_FP32_REVISION`. The download goes
    through `steno_speech::ModelStore` (resume, lock, mirror) into
    `fluidaudio/parakeet-tdt-0.6b-v3`, where `coreml_parakeet_installed` looks.
    This is a new path on an existing host, so the PR adds it to invariant 3
    in the Rust plan.
  - Map a stored `whisperkit-large-v3-turbo`, `parakeet-ultra` or `parakeet-de`
    to `parakeet-v3` when the settings load. The app shows a one-time notice
    ("Steno now transcribes with Parakeet v3"), and Settings no longer offers
    the three rows.
  - Processing fails with "Download the speech model in Settings" while the
    running engine's models are missing, instead of downloading them inside
    the pipeline. This is the second option in the parity line. "Process
    again" works once the models are installed.
  - Change the diarizer row's `display_name`, `source_repo` and
    `expected_bytes` to the ONNX pyannote segmentation 3.0 and WeSpeaker
    ResNet34-LM models, and show their licence notices with the other
    acknowledgements.
  - Tests: unit tests for the manifest, the mapping and the refusal. On Forge,
    download into an empty models directory, then check that the file tree and
    its SHA-256 list equal those of a directory the Swift app installed (the
    list is committed as a fixture), and that the CoreML backend transcribes
    the FLEURS sample exactly as before.
- **S2a Detection and auto-stop** (`feat/rust-recorder-policy`).
  - Port `DetectionController`: one prompt at a time, suppressed while
    recording or when the setting is off. The prompt goes through
    `panels::set_prompt` with its 60-second countdown, and `dismiss_prompt`
    reaches the controller.
  - Port the auto-stop after a call: the 90-second grace, "Keep recording" and
    the end reasons.
  - Tests: the Swift tests of both policies, ported as table tests against a
    fake clock. On Forge, a live run plays audio from a call app and checks
    that the prompt appears and the recording stops.
- **S2b Calendar and permissions** (`feat/rust-calendar-permissions`).
  - Look up the overlapping EventKit event at recording start (title and
    attendees). EventKit lives behind a trait; the macOS implementation
    follows the `unsafe` rule.
  - Add the audio crate's system-audio permission probe, and settle the rule
    for `unknown`: off the Mac it counts as not required; on the Mac it counts
    as missing until the probe records one sample. Then wire the host's
    `Permissions` to the shell's `permissions` in place of the fake.
  - Tests: the onboarding opener rule over the four states and three
    platforms. **Nicolai**: on a fresh macOS account, onboarding asks for each
    permission and a denied system-audio grant shows as missing.
- **S3 Update schedule and QR** (`feat/rust-update-schedule`).
  - The host's `Updater` over `updater.rs`: a daily automatic check (as
    `SUScheduledCheckInterval` 86400 did), stored automatic-check and
    automatic-download flags, and the last check time. The General section's
    Updates row reads the real values.
  - A QR crate draws the pairing code into the pairing snapshot.
  - Tests: the schedule against a fake clock (a check is due, skipped,
    retried after a failure). The QR image decodes back to the pairing payload
    in a test (a QR decoder as a dev-dependency). R3 proves the schedule on
    real releases.
- **S4 Handover on a changing network** (`fix/handover-republish`).
  - Re-register the Bonjour record when the interfaces change, on every
    platform.
  - The shell passes the computer name (`SCDynamicStoreCopyComputerName` on
    the Mac, `GetComputerNameExW` on Windows) to
    `HandoverConfiguration::default_service_name`.
  - Tests: a fake interface watcher that triggers re-registration.
    **Nicolai**: with a paired phone, switch the Mac from Wi-Fi to another
    network and back; `dns-sd -B _steno._tcp` shows the record each time, and
    the phone uploads without a restart.
- **S5 The Swift app's identity** (`feat/desktop-mac-identity`). These are the
  cutover plan's steps 1, 4 and 6, and step 5 as D4 decides it.
  - `tauri.conf.json` `identifier` becomes `uno.schmid.steno.mac`.
  - `apps/desktop/src-tauri/Info.plist` carries the Swift app's
    `SUPublicEDKey` (`RxaX7phoHvb7M0P4yaOC7zngDo+lqlOE6Iq89UtOuQI=`). It is
    inert in the Tauri app.
  - A macOS-only `swift_import` module in `steno-services` runs once at
    launch, before the host is built:
    - It copies the four `UserDefaults` keys into `preferences.json`, reading
      them through `cfprefsd` (`/usr/bin/defaults export uno.schmid.steno.mac -`
      parsed with the `plist` crate, so no `unsafe` is needed). It runs only
      while `preferences.json` does not exist.
    - It imports the handover identity: certificate by label,
      `SecIdentityCreateWithCertificate`, `SecItemExport` as PKCS#12, then the
      PEM entry `handover-identity`. It runs only while that entry is empty.
      If it fails, the app mints a new identity, and the release notes say
      that phones pair again.
    - It removes the Launch Agent a desktop-id build left behind
      (`tauri-plugin-autostart`'s plist), and registers `SMAppService` instead
      if that agent was enabled.
  - Tests:
    - A fixture plist covers the import, including a missing key and an
      existing `preferences.json`.
    - Behind `STENO_KEYCHAIN_TESTS=1` on Forge, in a throwaway keychain: the
      Swift CLI mints the identity under its label, the Rust import reads it,
      and the fingerprint and `macID` come out equal.
    - R2 proves the real access rules (an item the Swift app created, read by
      the Tauri app).
- **S6 Release mechanics** (`ci/desktop-stable-release`). The section Release
  mechanics gives the details.
  - Tags move to `v*`. Every macOS build gets a build number, and the
    workflow fails if it is not above the `appcast` branch's newest one.
  - The macOS bundle job writes a signed Sparkle item.
  - A stable tag publishes a full release that is GitHub's "latest", adds the
    item to the appcast and bumps the cask; the job summary shows the flake
    lines.
  - `release.yml` is deleted. The Swift scripts the desktop workflow keeps
    move to `apps/desktop/scripts/`, and `ReleaseScriptsTests` becomes
    `*.test.sh` files run by `rust-ci.yml`.
  - Tests: the shell tests, and one manual run on the branch with
    `platforms=macos`. That run's `steno-desktop-sparkle` artifact is what R1
    rehearses with.
- **S7 Site and README** (`docs/stable-release-pages`). Written during the
  release-candidate phase and merged only after the stable release is public
  (Landing page and README).
- **S8 Swift removal**. This is cutover step 7, after the rollback window.
  The web app also moves to `apps/web`, the Swift rows leave `AGENTS.md`, and
  issue #74 is re-scoped or closed.

## Release mechanics

### Tags and versions

A stable release is a merge commit that sets `[workspace.package] version` to
`X.Y.Z`, followed by a pushed `vX.Y.Z` tag. A release candidate is the same
with `X.Y.Z-rc.N`. The `plan` job requires the tag to be `v<version>` (today
it requires `desktop-v<version>`). Each tag sits on its own version-bump
commit, so no two tags share a commit count. The Swift rule "never two tags on
one commit" therefore holds by construction.

The build number is `git rev-list --count HEAD`. The `plan` and macOS jobs
check out with `fetch-depth: 0`, and the macOS build gets the number through
the configuration merge (`{"bundle":{"macOS":{"bundleVersion":"<n>"}}}`), the
same way the MSI version reaches WiX today. Before any bundle is built, `plan`
reads the highest `sparkle:version` on the `appcast` branch and fails if the
build number is not above it.

### The workflow after S6

- The trigger becomes `tags: ['v*']`, and the `desktop-v*` trigger goes. The
  release title becomes "Steno <version>", and the manifest URLs in
  `updater-manifest.sh` point at `releases/download/v<version>`.
- **macOS bundle job.** After "Notarise the disk image" (the stapled DMG is
  final, and the EdDSA signature covers its bytes), a "Sparkle item" step
  runs `generate_appcast` from a pinned Sparkle release (2.10.x, the version
  the Swift app links). The tarball is fetched with a SHA-256 pin, and
  `SPARKLE_PRIVATE_KEY` is in that step's environment only. The flags are
  `--download-url-prefix .../releases/download/v<version>/`,
  `--maximum-deltas 0` and the phased rollout from D8, so the item carries
  the build number, the short version, the minimum system version and
  `arm64` from the DMG itself (`make-appcast.sh` does the same). The item
  goes into its own artifact, `steno-desktop-sparkle`, because
  `release-assets.sh` rejects files it does not know, and the item is not a
  release asset. The job's Check secrets gains `SPARKLE_PRIVATE_KEY`, and the
  desktop README's secrets table lists it.
- **`publish`.** For a version with a hyphen, nothing changes:
  `--prerelease --latest=false`. For a version without one:
  - `gh release edit "$TAG" --draft=false --prerelease=false --latest`;
  - then `publish-appcast.sh "$TAG" false` with the downloaded item, run
    after the release is public, since only then does the enclosure URL
    resolve;
  - then the cask bump (`bump-homebrew-cask.sh`, with the URL scheme in the
    tap changed once by hand, see below);
  - then the Nix flake lines in the job summary.
  The lane releases stay pre-releases with `--latest=false`, so they never
  take "latest".
- **`release-notes.sh`.** A stable release gets the handoff paragraph:
  - Swift users receive this as an update;
  - phones stay paired, or pair again if the identity import failed;
  - desktop-id installs ask once for their permissions and keychain items
    (D5);
  - the optional mixdown is now WAV;
  - Whisper, Ultra and DE now transcribe with Parakeet v3.
  The Windows and OpenPGP paragraphs stay as they are.

### Lanes

`desktop-stable` and `desktop-beta` keep their names and rules
(`updater-lanes.sh`). The first stable tag creates `desktop-stable`. Until
then, a stable build's check of that lane fails with a 404; the only builds
that read it are the stable release and its successors, so this never happens
in practice. An installed `0.1.0-rc.2` reads `desktop-beta` and moves to
`0.11.0-rc.1` (D5).

### GitHub "latest"

A stable tag is the only thing that sets `--latest`. From the first stable
release on, `releases/latest` resolves, and so do the site's and the README's
links. The README badge drops `include_prereleases`.

### `release.yml`

It is deleted in S6, before the first `v0.11.0*` tag. That also closes the
cross-workflow concurrency item. These scripts move to `apps/desktop/scripts/`
with their tests: `publish-appcast.sh`, `merge-appcast.py` and
`bump-homebrew-cask.sh`, which must run on Ubuntu (`sha256sum` where
`shasum` is missing). If the rehearsal shows that a Swift build cannot take
the handoff and a Swift bridge release is needed, a dedicated PR restores
`release.yml` with a `swift-v*` trigger. That bridge release must ship before
the handoff item exists: the item has to carry the highest `sparkle:version`
on the appcast, and `plan`'s check enforces that only for Tauri builds.

### Homebrew and Nix

**Nicolai** sets `HOMEBREW_TAP_TOKEN` (D7) and changes `Casks/steno.rb` once
by hand, together with the first stable bump:

- `url ".../releases/download/v#{version}/Steno_#{version}_aarch64.dmg"`;
- the comment about Sparkle and `releases/latest` goes;
- `livecheck` keeps stable versions only (`(\d+(?:\.\d+)+)$`);
- `zap` adds the Tauri directories named after the bundle id
  (`~/Library/WebKit/uno.schmid.steno.mac`,
  `~/Library/Application Support/uno.schmid.steno.mac`) and
  `~/Library/Caches/uno.schmid.steno.mac`;
- the caveat says that Steno updates itself.

`auto_updates true` stays.

`flake.nix` gets the same URL change in S6. R7 checks that `undmg` unpacks
the Tauri DMG; the bundler builds HFS+ images, which `undmg` reads, unlike
APFS. The flake bump stays a manual PR, made from the job summary.

## The Sparkle handoff

### Who receives it

Every Swift install: `v0.9.0-rc.1` to `v0.10.0-rc.2`, Sparkle 2.10, the feed
`https://raw.githubusercontent.com/NicolaiSchmid/steno/appcast/appcast.xml`.
They receive it on the daily check after the first stable release adds its
item, one rollout group per day (D8), or at once from Check for Updates. Nix
installs see the item but cannot install it from the read-only store; they
move by bumping the flake.

### The item

The stable release's item is generated as above and added on top of the
rolling appcast by `publish-appcast.sh` without `--channel`:

- `sparkle:version` is the commit count (above 542);
- `sparkle:shortVersionString` is `0.11.0`;
- `minimumSystemVersion` is 15.0 and `hardwareRequirements` is `arm64`;
- the enclosure is `https://github.com/NicolaiSchmid/steno/releases/download/v0.11.0/Steno_0.11.0_aarch64.dmg`,
  with its length and `sparkle:edSignature` from `SPARKLE_PRIVATE_KEY`.

The older Swift items stay on the branch. Sparkle offers the highest build a
host can take, so they are never offered again.

### What Sparkle checks, and how the Tauri bundle passes

| Sparkle check | Tauri bundle |
|---|---|
| The archive's EdDSA signature verifies against the running app's `SUPublicEDKey` | Signed with the same `SPARKLE_PRIVATE_KEY` |
| The new bundle keeps a public key (Sparkle supports rotating keys, not removing them) | S5 puts the same `SUPublicEDKey` in `Info.plist` |
| The new bundle's identifier matches the host's | S5: `uno.schmid.steno.mac` |
| The new bundle's code signature is valid (and would have to match the designated requirement if the keys differed) | Developer ID, team `KQB68F43PW`, hardened runtime, the sidecar signed inside, notarised; `check-bundle.sh --signed` checks it |
| The item's build number is above the host's | The commit count, checked against the appcast in `plan` |
| Minimum system version and architecture | 15.0 and `arm64`, as in Swift |

Sparkle mounts the DMG, takes `Steno.app` out of it, replaces the running
bundle at its own path (usually `/Applications/Steno.app`; the old one goes to
the Trash) and relaunches. The executable's new name (`steno-desktop`) does
not matter: Sparkle relaunches the bundle, TCC keys on the bundle id and team,
and the keychain trusts the designated requirement, which does not name the
executable.

### First launch after the handoff

The Tauri app starts at the same path, with the same bundle id and the same
designated requirement as the Swift app. In order:

1. **Data.** `~/Library/Application Support/Steno/` is shared and its schema
   is the same, so the meetings are simply there. The app's own Tauri
   directories (WebKit data, panel anchor) start empty under the bundle id.
2. **Preferences.** `swift_import` copies `steno.onboardingCompleted`,
   `steno.systemAudioGranted`, `steno.loginItemRegistered` and drops the panel
   anchor, so onboarding does not reopen.
3. **Secrets.** The API key is read through `keyring` under the same
   service and account (the Codex sign-in stays in the Codex CLI's own
   `auth.json`). The items are in the file keychain
   (Swift never used the data-protection keychain), and their access list
   trusts the designated requirement and the team's partition, so no prompt
   appears. R2 proves this.
4. **Handover identity.** It is imported into `handover-identity`, so `macID`
   and the pinned fingerprint stay the same and the phones keep uploading.
   The Swift keychain item stays in place.
5. **Login item.** The Swift `SMAppService.mainApp` registration launches the
   new app at login, and the General section reads it through the same API
   (D4). One entry stays in Login Items.
6. **Permissions.** Microphone, system audio and calendar grants carry over
   (same bundle id and requirement). R2 checks for the absence of any prompt.
7. **Updates.** From now on the Tauri updater checks `desktop-stable` daily
   (S3). Sparkle is gone. Its cache under
   `~/Library/Caches/uno.schmid.steno.mac/org.sparkle-project.Sparkle` is
   harmless and stays.

### Installs of the desktop-id build

These are covered in D5. They convert through `desktop-beta` at
`0.11.0-rc.1`, not through Sparkle. Their Launch Agent is replaced by
`SMAppService` (S5). A Mac that ran both apps from two different paths ends
up with two copies of the Tauri app on one database; the release notes tell
those users to delete the one outside `/Applications`.

## Rehearsal

Nothing reaches Swift users until every step below has passed on the
release candidate the stable release will be built from. Each step names who
runs it and where.

**Forge** is `ssh nschmid10049@forge`: no `sudo`, shared with other
projects' CI. Work goes under `~/steno-handoff/` and never under
`~/steno-calibration/`, with no app in `/Applications`, and every process
started there is stopped by the PID that was recorded for it. Forge's login
shell has no `gh`, so assets are fetched on atlas and copied over with `scp`.

**A fresh account** is a new macOS user that Nicolai creates, on Forge or on
his own Mac. TCC grants, keychains and login items are per user, so such an
account behaves like a real install.

- **R1 Handoff mechanics, headless** (agent, Forge; also the cutover plan's
  test 6). Uses Sparkle's own `sparkle-cli`, which runs the installer and the
  checks the in-app updater runs.
  1. Copy `Steno.app` from the `v0.10.0-rc.2` DMG to
     `~/steno-handoff/apps/`, and save `codesign -dr -` of it.
  2. Put the candidate DMG and its `steno-desktop-sparkle` item in
     `~/steno-handoff/feed/`. Rewrite the item's enclosure URL to
     `http://127.0.0.1:8765/<dmg>`; the EdDSA signature covers the file, not
     the URL. Serve the directory with `python3 -m http.server 8765 --bind
     127.0.0.1` and record the PID.
  3. Run `sparkle.app/Contents/MacOS/sparkle ~/steno-handoff/apps/Steno.app
     --feed-url http://127.0.0.1:8765/appcast.xml --check-immediately
     --verbose`. Check the flags against the `--help` of the pinned Sparkle
     version first. If the installer cannot be reached from an SSH session,
     run it in Forge's logged-in session instead.
  4. Pass when:
     - the bundle at that path now has `CFBundleExecutable` `steno-desktop`,
       `CFBundleIdentifier` `uno.schmid.steno.mac`, a `CFBundleVersion` above
       542 and the Swift `SUPublicEDKey`;
     - `codesign -dr -` prints the saved requirement;
     - `codesign --verify --deep --strict` passes;
     - `spctl -a -t exec -vv` says "Notarized Developer ID".
  5. The same run with a damaged `edSignature` leaves the app untouched.
     With `sparkle:version` 500, the run finds no update.
  6. A `v0.9.0-rc.4` copy (build 450), with a feed made of the `appcast`
     branch plus the candidate item, takes the candidate.
  7. Stop the server by its recorded PID.
- **R0 Desktop-id conversion** (Nicolai, fresh account). Install
  `desktop-v0.1.0-rc.2`, enable launch at login, pair nothing. When
  `0.11.0-rc.1` is on `desktop-beta`, check for updates and install. Pass
  when:
  - the app relaunches as `uno.schmid.steno.mac`;
  - Login Items shows one Steno;
  - the TCC prompts appear once, as D5 says;
  - the meetings are listed.
- **R2 The real handoff** (Nicolai, fresh account; the cutover plan's tests 1
  to 3).
  1. Install `v0.10.0-rc.2` from its DMG. Grant every permission, set an API
     key, pair the phone, record and process a meeting, and turn on launch
     at login.
  2. Point Sparkle at the R1 feed with `defaults write uno.schmid.steno.mac
     SUFeedURL http://127.0.0.1:8765/appcast.xml`, and check that the release
     build honours it. If it does not, run `sparkle-cli` on the installed
     app, which loses only the menu path.
  3. Check for Updates and install.
  4. Pass when:
     - the app comes back as the Tauri app;
     - the meeting is listed;
     - a summary runs with the stored key and no keychain prompt;
     - onboarding stays closed;
     - Login Items shows one Steno, and after logging out and back in, Steno
       starts once;
     - a call records both lanes with no TCC prompt;
     - the phone uploads a recording without pairing again;
     - Settings shows the CoreML Parakeet as installed.
- **R3 Updates after the handoff** (agent tags, Nicolai watches; the
  cutover plan's test 4). Publish `0.11.0-rc.N+1`. Without being asked, the
  R2 app finds it within one scheduled check, installs it and relaunches, and
  processing a meeting still starts the sidecar from the new bundle.
- **R4 Fresh install** (Nicolai, fresh account; agent for the soak; the
  cutover plan's test 5).
  1. The DMG passes Gatekeeper offline (`xcrun stapler validate`, `spctl`).
  2. Onboarding asks for each permission.
  3. Settings downloads the CoreML Parakeet and the diarizer models with
     progress.
  4. A five-minute call is transcribed, diarized and exported.
  5. On Forge, under `nice`, the CLI processes a two-hour synthetic two-lane
     recording, and the peak resident memory is recorded (What follows).
- **R5 Offline Swift build.** This is step 6 of R1.
- **R6 Rollback drill** (Nicolai, the R2 account; agent for the feed part).
  1. Drag `v0.10.0-rc.2` back over the Tauri app. It opens the same
     database, lists every meeting (the meetings recorded with the Tauri app
     included) and reads the key. Then put the Tauri app back.
  2. On a scratch branch, `APPCAST_BRANCH=appcast-rehearsal
     publish-appcast.sh` adds the item, and `git revert` of that commit takes
     it out again. A feed read from that branch then offers nothing.
- **R7 After the stable tag** (agent).
  - `gh api repos/NicolaiSchmid/steno/releases/latest --jq .tag_name` answers
    `v0.11.0`.
  - `desktop-stable/latest.json` names 0.11.0.
  - The `appcast` branch's top item is 0.11.0, with no channel and with the
    phased rollout.
  - `brew install --cask --appdir=~/steno-handoff/apps steno` on Forge
    installs the Tauri app.
  - `nix build github:NicolaiSchmid/steno#steno` unpacks it.
- **Dogfood** (Nicolai, his own account). After R0 to R6 pass, his daily
  Swift install takes the candidate through the R2 feed. Pass: at least five
  real meetings recorded, processed and exported over at least three days,
  one phone upload, and no regression he would not ship.

## Landing page and README

S7 is merged after R7 passes.

- **`apps/site/src/lib/site.ts`**: `download` stays `releases/latest`, which
  now resolves.
  - `mac`: `released: true`, note "Apple Silicon · notarised" (unchanged).
  - `linux`, if its gate passed: `released: true`, note
    ".deb · AppImage · OpenPGP-signed".
  - `win`, once its gate passed: `released: true`, note
    "x64 · installer not code-signed".
  The download button needs no change: it already links a released platform
  to `site.download`.
- **`how-its-built.tsx`**: the closing paragraph says that every download is
  the Rust version, and that on the Mac it replaced the original Swift app
  as an ordinary update and opened its meetings, settings and paired phones
  where they were. Proposed text: "Every desktop runs the same Rust app. On
  the Mac it replaced the original Swift app as an ordinary update, with your
  meetings, settings and paired phone where you left them." The subhead loses
  "is moving".
- **`open-source.tsx`**: "Swift Mac app · Rust core · Tauri shell" becomes
  "Rust core · Tauri shell".
- **`page.tsx`**: the JSON-LD `operatingSystem` lists the released platforms.
- **README**:
  - The status banner says stable and links the release.
  - The badges: release without `include_prereleases`, Rust CI in place of
    Swift CI, and the platform badge names the released desktops.
  - The install section has one block per OS:
    - macOS: Homebrew, the DMG, Nix. Updates come through the app; the Nix
      store cannot be written, and S3's automatic-check switch in Settings
      silences the checks, replacing the `defaults write ...
      SUEnableAutomaticChecks` line.
    - Windows: the MSI or `-setup.exe`. Neither is code-signed, so SmartScreen
      asks first (More info, Run anyway). The hash can be checked against
      `SHA256SUMS`.
    - Linux: `.deb` or AppImage, verified with the `gpg --verify` and
      `sha256sum --check` lines from the desktop README's "Checksums and
      OpenPGP signatures". That section's key URL moves from the
      `desktop-v<version>` tag to `v<version>`.
  - "For developers" puts the Rust workspace first; the Swift lines go in S8.
  - The rest of the README's Mac-only wording is left for a docs pass after
    S8.
- **`apps/desktop/README.md`**: "Cutting a release" and "Publishing, on a
  tag" use `v<version>` and the full release. "A bad release" covers the
  first stable release (below). "Not here yet" loses what S1 to S5 close.
- **Comments**: the "never latest" comments in `updater.rs` and at the top of
  `desktop-release.yml` go.

## Order of operations and gates

1. **Nicolai confirms D1 to D8** on this PR.
2. **S1, S2a, S2b, S3 and S4 run in parallel.** Each goes through the
   pipeline. S6 can be written meanwhile; its manual run must pass before it
   merges.
3. **S5 merges, then S6.** No tag is pushed between the two merges.
4. **Gate G1:** S1 to S6 are merged, the blocking rows above are ticked in
   the Rust plan's parity list, and Rust CI is green on all three platforms
   at the bump commit.
5. **Bump to `0.11.0-rc.1` and tag `v0.11.0-rc.1`.** This publishes a
   pre-release, `desktop-beta` moves, and no appcast item is written.
   Desktop-id installs convert.
6. **Gate G2:** R1, R4 and R0 pass, run by the agent and Nicolai. Every fix
   becomes `rc.N+1`, and the steps it touches run again.
7. **Gate G3:** R2, R3 and R6 pass on the candidate, then the dogfood.
8. **Bump to `0.11.0` on a fresh commit and tag `v0.11.0`.** This publishes
   a full release that is GitHub's "latest", creates `desktop-stable`, adds
   the appcast item, and bumps the cask (with the token, or by hand on the
   same day). Open the flake bump PR.
9. **Gate G4:** R7 passes. Merge S7; Vercel deploys the site from `main`.
10. **Rollback window: 14 days.** No database migration merges, so the Swift
    app can still open the database. Watch the issues. Linux and Windows
    become released on the site when their gates pass (D6).
11. **S8**, the Swift removal.

The Linux gate at the stable tag:

- S1 and S4 are merged (they are Linux blockers too);
- **Nicolai** runs one GNOME session on hardware: install the `.deb`, record
  a call, log out and confirm that the recording was saved; the AppImage
  starts;
- the release notes list the open **First Linux release** items as known
  issues (the KDE and Xfce-on-Wayland logout, the whole default sink, no
  meeting detection on Linux, the WebKitGTK descriptor leak).

The Windows gate: a Windows machine runs the `--ignored` WASAPI tests and one
real call. Until then the installers are on the release but the site says
"Not released yet".

## Rollback checklist

- **Before step 8**, nothing reaches Swift users or stable desktop installs.
  A bad candidate is handled by the desktop README's "A bad release", for
  `desktop-beta`.
- **A bad handoff item, while it is still rolling out:**
  1. Run `git revert` on the `appcast` branch's commit for the item and push.
     Swift installs that have not taken it stop seeing it at their next
     check, and the phased rollout keeps that group small.
  2. If fresh installs are affected too, run
     `gh release edit v0.11.0 --prerelease`, which takes the release out of
     "latest", and in the same hour set the site's affected platform back to
     `released: false`. Otherwise the Download button leads to a 404 again.
  3. Revert the tap commit and the flake bump.
- **Users who already took a bad release** cannot go back through Sparkle.
  Fix forward with `0.11.1`, which the daily check from S3 delivers (R3
  proved it). To stop the spread, delete `latest.json` from `desktop-stable`
  (`gh release delete-asset desktop-stable latest.json`): the first stable
  release has no earlier release on that lane to put back. `desktop-beta`
  gets the last good candidate's manifest. Never re-run the bad tag's
  publish.
- **As a last resort for one user:** drag the `v0.10.0-rc.2` DMG over the
  app. It uses the same database and keychain (R6 proved it). The appcast
  item must be reverted first, or the Swift app offers the handoff again.
- **The database:** no migration merges in the window, and every migration
  after it is mirrored in `Migrations.swift` until S8, so going back stays
  possible until the Swift app is removed.

## Changes to other plans in this PR

- **`.plans/2026-10-04-mac-cutover.md`**: a note at the top says this plan
  extends it and which parts it replaces.
- **`.plans/2026-10-02-rust-core-and-tauri-shell.md`**:
  - the status line, the WP9b progress row and the owner sentence of "Open
    after the port" point at this plan;
  - the closed **WP9b.** item about the first `desktop-v*` tag is deleted;
  - the other **WP9b.** items stay until the pull requests that fix them
    delete them.
