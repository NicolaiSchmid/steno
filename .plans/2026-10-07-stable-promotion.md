# Stable promotion: the Tauri app becomes the Steno release

Status: planned 2026-10-07, not started. Nicolai confirmed every decision on
2026-10-07: D1, D2, D4, D7, D8, D10, D12 and D13 as proposed, D3, D5, D6 and D9
with the changes the plan follows, and D11 as his own. The decision table at the
head of "Decisions" shows what changed.

Nicolai decided on 2026-10-07 to promote the Tauri app from beta to stable:
release tags and the build pipeline produce the Tauri app, the landing page
offers it, and existing users of the Swift Mac app end up on it without losing
meetings, settings, secrets, paired phones or the login item. His rule for the
whole promotion: no data is ever lost (D3).

This plan extends `.plans/2026-10-04-mac-cutover.md` and changes it in these
places:

- the opening gate (every unticked parity line) becomes the blocking list below
  (D3), and its one pull request becomes the packages below;
- step 1's bundle id becomes `com.nicolaischmid.steno.desktop` (D5), so the app no longer
  keeps the Swift app's id;
- step 2's beta staging and signing, and all of step 3 (distribution), become
  "Release mechanics" and "The Sparkle handoff"; step 2's frozen `appcast`
  branch stands, with one handoff item (D8);
- step 4 reads the Swift app's preference domain explicitly, copies one key
  and Sparkle's two update flags, and drops the panel anchor (S6);
- step 5's open choice is D4, which keeps `SMAppService`; the new app registers
  itself, and the Swift app's entry is handled as S6 says;
- step 6 also replaces an identity that a desktop-id build stored (D5), and
  expects a keychain prompt;
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

Checked on 2026-10-07 against `main` at `2f9a253b` (#217's merge),
the releases, the `appcast` branch, the released bundles, the GitHub API, the
Sparkle 2.10.0 source and tarball, GRDB 7.11.1, the Tauri 2.10 bundler, Apple's
Security sources, Omarchy 4.0.4 and nixpkgs. The rollback facts were run on
Forge and atlas.

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
  version ("0.1.0-rc.2"), which Sparkle ranks below 542. `main` has well over
  2068 commits, so the commit count, which is the Swift scheme, is far above 542.
  Tauri 2.10's bundler takes `bundle.macOS.bundleVersion`.
- **Sparkle hands over across bundle ids.** In Sparkle 2.10.0:
  - the installer finds the new app in the archive by file name (`Steno.app`),
    and by bundle id only as a fallback (`Autoupdate/SUInstaller.m`);
  - the validator has no check that the new bundle's id equals the host's; its
    only id check compares the old host bundle with the id the installer was
    started for (`Autoupdate/AppInstaller.m`);
  - with the archive's EdDSA signature verified against the running app's key,
    the new app's code signature only has to be valid, not match the old
    designated requirement (`Sparkle/SUUpdateValidator.m`);
  - the new bundle must still carry a public key: Sparkle supports rotating the
    key, not removing it;
  - the new app lands at the host's path, whatever its id.

  R2, R7 and R8 check this on the real bundles.
- **What macOS ties to the bundle id** (D5):
  - TCC grants (microphone, system audio, calendar) are stored per bundle id, so
    none carry over and every Swift user grants them once more;
  - the API key is one keychain item that both apps read (same service and
    account, `KEYRING_SERVICE`). Its access list trusts the Swift app. When the
    new app reads it, macOS asks, and the dialog asks for the login keychain
    password (Apple's `securityd` always requires it for this prompt). "Always
    Allow" puts the new app on the item's list for good; "Allow" lets this one
    read through and asks again next time. `keyring` writes such an item in
    place and keeps its list, so writing it again changes nothing;
  - the handover identity's private key was imported with the default access,
    which trusts only the Swift app, so exporting it asks too ("wants to export
    key", with the password);
  - the `SMAppService.mainApp` registration belongs to the Swift bundle id, and
    an app can only register or unregister itself. What the stale entry does
    after the swap is not documented; Apple keeps stale entries "to preserve
    user intent";
  - the Swift app's preferences stay in the `uno.schmid.steno.mac` domain.
- **Where the identifier names things.** The support directory, with the
  database, audio and models, is not named after it on any platform
  (`StenoPaths`: `~/Library/Application Support/Steno`, `~/.local/share/Steno`,
  `%APPDATA%\Steno`). Tauri's app config directory, where `panel-anchor.json`
  lives (`panels.rs`), and the webview's data are: on macOS
  `~/Library/Application Support/<id>` and WebKit's `~/Library/WebKit/<id>`, on
  Linux `~/.config/<id>` and `~/.local/share/<id>`, on Windows `%APPDATA%\<id>`
  and `%LOCALAPPDATA%\<id>`. The web app keeps nothing in webview storage. The
  single-instance guard is named after it on every platform. On Windows it also
  sets the AppUserModelID (taskbar pins), while the MSI upgrade code and the
  NSIS registry keys come from the product name, so an installer with a new
  identifier still upgrades in place; the NSIS uninstaller's opt-in data cleanup
  removes only its own identifier's folders.
- **The keyring service is not the identifier.** Every Steno keyring entry is
  filed under the service string `uno.schmid.steno.mac` (`KEYRING_SERVICE`), in
  the Keychain, in the Windows credential store, and on Linux in the Secret
  Service once #221 lands. It stays, so no secret moves.
- **`com.nicolaischmid.steno` is the iOS app's production bundle id**
  (`mobile/app.config.ts`). App Store Connect offers an iOS app on Apple
  silicon Macs, so a desktop app with the same id would collide with it (TCC
  rows, Launch Services). The desktop app therefore takes
  `com.nicolaischmid.steno.desktop` (D5), and the iOS app's availability needs
  no change.
- **Both desktop pre-releases published without manual steps** (runs
  37349614159 and 37425455820). That covers the `publish` job and an MSI for an
  `-rc.N` version, so the **WP9b.** item about the first `desktop-v*` tag is
  closed.
- **The phone finds the computer by `macID`, not by name.** `macID` is derived
  from the handover certificate's fingerprint (`HandoverIdentity::mac_id`), and
  the phone looks the service up by it (`findByMacID`). A new identity therefore
  breaks every pairing. At launch, before any window exists, the app reads the
  API key and loads or mints `handover-identity` (`app.rs`, `handover_listener`);
  a desktop-id build (`uno.schmid.steno.desktop`, the beta's identifier) has
  minted its own that way.
- **The handover listens on a port the system picks** (`port: 0`) and advertises
  through its own mDNS responder (`mdns-sd`). A firewall that blocks incoming TCP
  hides it from the phone; `ufw` admits mDNS by default.
- **The Rust app has no "Process again".** "Try again" re-runs only the summary
  and is off without a transcript. The pipeline can process a meeting again
  (`ProcessingPipeline::reprocess`), but neither the bridge nor the CLI calls it
  yet, and the CLI reads WAV only. P9 adds them.
- **A recording ended by a kill is marked failed at the next launch**
  (`fail_interrupted_recordings`), and nothing salvages its CAF yet (P3).
- **Omarchy 4** (Arch with Hyprland, Wayland):
  - it is a `uwsm` session: logout is `uwsm stop`, which stops the app's unit
    with SIGTERM while Hyprland still runs, so the app's signal handler saves;
    reboot and poweroff go through logind, whose delay lock Omarchy sets to 15 s,
    and Omarchy kills the user manager 5 s into its stop, so at a reboot the
    save must end within logind's 15 s delay;
  - systemd's XDG autostart generator gives the autostarted app's unit
    `TimeoutStopSec=5s`, so a save longer than five seconds is killed; this
    holds on every systemd desktop that runs XDG autostart through it;
  - `app.slice` is under systemd-oomd with `kill`, and the app and its speech
    sidecar share one cgroup, so a memory-pressure kill takes the recording;
  - an app started outside a `uwsm` scope sits in Hyprland's own unit, stopped
    with it after at most 10 s;
  - Hyprland's portal serves no Inhibit and no session monitor;
  - the bar is a Quickshell shell, whose tray is an SNI host behind a hover
    drawer; with `libayatana-appindicator` a left click on Steno's icon probably
    does nothing (libayatana answers `Activate` with `UnknownMethod`), and a
    right click opens the menu;
  - it sets `GDK_BACKEND=wayland,x11,*`, which the shell today treats as the
    user's choice, so the panels run as native Wayland windows that Hyprland
    places itself, never reporting moves; on XWayland Hyprland floats them and
    takes their position. Neither backend honours "above" or "all workspaces",
    and a new window takes focus unless a rule says not to;
  - window rules are Lua (`o.window(...)` in `~/.config/hypr/`);
  - `ufw` denies every incoming port but LocalSend's 53317;
  - gnome-keyring runs with a plaintext, always-unlocked default keyring, which
    a multi-line secret corrupts (omacom/omarchy #5105, duplicate #11159).

  These come from Omarchy's and Hyprland's source; the Omarchy gate runs them.
- **NixOS:** a Tauri app needs `wrapGAppsHook3`, without which GTK's file chooser
  aborts the process for want of its schemas; `ort` must link nixpkgs'
  `onnxruntime` (`ORT_LIB_LOCATION`, `ORT_PREFER_DYNAMIC_LINK=1`), since the
  sandbox forbids the build-time download; the tray library is loaded with
  `dlopen`, so its path is patched; an autostart entry that names the store
  path breaks after garbage collection, and plain Hyprland without `uwsm` runs
  no XDG autostart at all. nixpkgs' `handy` (a Tauri 2 app with ONNX) shows the
  ORT variables, the `dlopen` patch and `cargo-tauri.hook` (with
  `wrapGAppsHook4`).
- **Asset names differ.** The Swift release carries `Steno-<v>.dmg`. The Tauri
  release carries `Steno_<v>_aarch64.dmg` and `.app.tar.gz`,
  `Steno_<v>_x64_en-US.msi` and `_x64-setup.exe`, and
  `steno-desktop_<v>_amd64.deb` and `.AppImage`. The cask and `flake.nix` build
  the Swift URL, and the flake checks for an executable named `Steno` and the
  bundle id `uno.schmid.steno.mac`.
- **Rollback over a v5 database** (run on Forge and atlas):
  - Swift and Rust stand at migrations 1 to 4. P2 adds a fifth in both apps, a
    new table with no foreign key and no change to existing tables;
  - Swift `v0.10.0-rc.2` (GRDB 7.11.1) opens a v5 database normally: the
    migrator ignores an applied migration it does not know, never erases while
    `eraseDatabaseOnSchemaChange` is false, and the app never calls
    `hasBeenSuperseded`. It then keeps writing, and nothing is lost;
  - the Rust `desktop-v0.1.0-rc.2` refuses it (`UnknownMigration`), then panics
    at launch with no dialog. The file is untouched, but the in-app updater
    cannot run, so that install needs a newer build installed by hand. P2 makes
    the Rust migrator ignore later migrations with a warning, as GRDB does, and
    show a dialog instead of panicking.

## Decisions

| | Before 2026-10-07 | Nicolai's answer | Now |
|---|---|---|---|
| D1 | `v*` tags | as recommended | Confirmed |
| D2 | 0.11.0 | as recommended | Confirmed |
| D3 | the blocking list gates data loss, core flows and silent failures | "we should never loose any data" | Changed: every data-loss path blocks (P1 to P38) |
| D4 | `SMAppService`, the Swift registration carrying over | as recommended | Confirmed; with D5 the app registers itself (S6) |
| D5 | `uno.schmid.steno.mac` on the Mac, `uno.schmid.steno.desktop` elsewhere | "it should be com.nicolaischmid.steno", then "Desktop gets .desktop" | Changed: `com.nicolaischmid.steno.desktop`; directories decoupled; confirmed |
| D6 | Linux and Windows listed once gated | "also omarchy and nixos users" | Changed: GNOME, Omarchy and NixOS, X1 to X8 |
| D7 | the cask follows stable releases only | as recommended | Confirmed |
| D8 | one frozen handoff item | as recommended | Confirmed |
| D9 | the parity differences accepted | "audio processing stable and ideally the final version" | Changed: A1 to A9 before the first candidate; the final choices confirmed |
| D10 | `steno-macos` | as recommended | Confirmed |
| D11 | none | the lost-complete fix before stable | New, decided by Nicolai |
| D12 | the dogfood: five meetings in three days | confirmed as proposed | New, confirmed |
| D13 | none | "Write beside" | New, confirmed |

A confirmed decision states the choice. A changed or new decision also states
why and the alternative.

- **D1 Tags are `v*` for the Tauri app.** Confirmed. One product, one tag line.
  `release.yml` goes in S7, before the first `v0.11.0*` tag. The updater lanes
  keep the names `desktop-stable` and `desktop-beta` for good, because installed
  desktop builds have them compiled in. The old `desktop-v*` tags stay.
- **D2 The version line continues at 0.11.0.** Confirmed. Candidates are
  `0.11.0-rc.N`; `1.0.0` remains the name for the day the v1 checklist (issue
  #74, re-scoped to the Rust app) is done.
- **D3 Nothing that can lose data ships.** Changed. Nicolai: "we should never
  loose any data. if there are gaps, then we need to dispatch more agents."
  Every known path that can lose a recording, a transcript, a note or a pairing
  blocks the stable release, whatever its likelihood: in the Rust app on every
  platform the release publishes, Windows included though unlisted, and in both
  apps where they share it. A
  Swift-only path closes with the handoff (D9), since Swift ships no further
  release and runs again only as a rollback. Everything that loses no data
  follows (What follows). The blocking list holds the paths found in the plans
  and by the data-loss audit of the Rust app's code. Alternative: the earlier gate, which also
  blocked core flows and silent failures but not every unlikely loss.
- **D4 The login item stays `SMAppService.mainApp` on macOS.** Confirmed. It is
  the Mac's own login item, the one the Swift app uses, and it keeps the General
  section's `requiresApproval` copy. The shell registers, reads and removes it
  through the safe `smappservice-rs` (already in `Cargo.lock`, moved into
  `[workspace.dependencies]`), not through `tauri-plugin-autostart`'s Launch
  Agent. With D5, the new app registers itself (S6). Linux and Windows keep the
  plugin, with X3's changes.
- **D5 The app's identifier becomes `com.nicolaischmid.steno.desktop`.**
  Changed, confirmed 2026-10-07. Nicolai: "it should be com.nicolaischmid.steno",
  then, on the iOS app sharing that id: "Desktop gets .desktop."
  - **One identifier on every platform,** `com.nicolaischmid.steno.desktop`,
    replacing both old ones, set in `tauri.conf.json`. The iOS app keeps
    `com.nicolaischmid.steno`. Alternative, not taken: per-platform
    identifiers.
  - **What it costs, stated plainly:**
    - every Swift user grants microphone, system audio and calendar once more;
    - macOS asks once for the API key and once for the handover key, each time
      with the login keychain password, and the user must choose Always Allow;
    - the Swift Login Items entry is left to macOS, and R3 finds out what it
      does (S6);
    - Tauri's per-identifier directories start empty, and the one thing in them
      that matters, the panel anchor, is read back once;
    - on a Mac that also holds a Swift identity, phones paired only with a
      desktop-id build pair again, since the Swift identity wins (S6).
  - **Directories: decouple, do not move.** Confirmed 2026-10-07 ("Stop
    depending on the ID"). The panel anchor
    moves into the support directory (`Steno/panel-anchor.json`), read once from
    the desktop-id build's config directory when the new file is missing. Then
    nothing that matters is named after the identifier, and a later id change
    costs nothing. Alternative, not taken: move the old directories once at
    first launch.
  - **Mobile's native identifiers are not touched.**
- **D6 Linux targets GNOME, Omarchy and NixOS; Windows ships unlisted.**
  Changed. Nicolai: "yes. but i also wanna target omarchy and nixos users."
  Every tag publishes all three platforms. The site lists a Linux target once
  its gate passes on Nicolai's machine: GNOME (the `.deb` or the AppImage),
  Omarchy (the AUR package `steno-desktop-bin`) and NixOS (the flake's package
  and module). Windows is listed once a Windows machine has run its gate. A
  gate blocks its target's listing, not the stable tag. Alternative: GNOME
  alone at first, Omarchy and NixOS later.
- **D7 The cask follows stable releases only.** Confirmed. The first stable bump
  is **Nicolai**'s tap commit; after it he sets `HOMEBREW_TAP_TOKEN`, and the
  workflow bumps from the second stable release on.
- **D8 One frozen handoff item.** Confirmed. The `appcast` branch gets one item
  without a channel, for `0.11.0`, with Sparkle's phased rollout
  (`--phased-rollout-interval 86400`: seven groups, one day apart, counted from
  the approval; Check for Updates skips the wait). The same feed is the
  `appcast.xml` asset of every stable release, so `v0.9.0-rc.1` finds it through
  `releases/latest`. Only a later release that fixes the handoff itself
  publishes a new item, which supersedes this one.
- **D9 The audio path is final before the first candidate, and the remaining
  Swift fixes close at the handoff.** Changed. Nicolai: "we need to make sure
  the audio processing is stable and ideally the final version before we
  cutover to the tauri version."
  - **Reading:** no change to the audio and speech path is planned after the
    cutover. A1 to A9 land before `0.11.0-rc.1`, so the rehearsals run on the
    final pipeline. A fix found during the candidates becomes a new candidate
    and restarts the stability count (G2).
  - **The remaining differences, each made final.** Confirmed 2026-10-07.

    | Difference | Choice | Why |
    |---|---|---|
    | Mixdown format | Final: 16 kHz mono WAV on every platform | One code path and no encoder dependency; the mixdown is optional (`include_audio`). Matching Swift's AAC needs AudioToolbox on the Mac, Media Foundation on Windows and nothing exists for Linux, so the format would differ by platform. Cost: about 115 MB per hour in the vault instead of about 30. Alternative: AAC on the Mac and Windows, WAV on Linux. |
    | Resampler (44.1 kHz phone audio) | Final as it is, proven by A9 | Within 0.3 dB to 6 kHz; A9's sweep and speech tests show no aliasing into the speech band. |
    | Sidecar's 2 ms lag | Final, accepted | Far below a word; Swift had the same relationship. |
    | AAC priming (23 to 48 ms late on phone recordings) | Fixed first (A9) | Reading the container's edit list is small and makes the decode exact, as AVFoundation's was. |
    | Call mode without an output client (the tap's IOProc runs only once another client opens the output) | Checked first on the Mac in a GUI session (A9) | If a call's first seconds are lost, the remedy becomes an A-package before the first candidate; otherwise final. |

  - The Swift defects under "Store", "Adapters", "Handover", "LLM" and "Audio"
    and the CLI's `--title`, and the parity notes' other "before cutover" ports
    to Swift, close with the handoff. If a bridge release ships (Release
    mechanics), it carries them. The fixtures the Swift side owes ("Bridge") are
    dropped at S9, when the Rust fixtures become the contract.
- **D10 A new crate, `steno-macos`, joins `AGENTS.md`'s `unsafe` list, and the
  list is brought in line with the code.** Confirmed.
  - Two packages call Mac frameworks that have no safe binding: the identity
    export (`SecItemExport`, S6) and EventKit (S3); the import's other keychain
    calls use `security-framework`'s safe wrappers. `steno-macos` holds both,
    each in a safe wrapper with a comment on every invariant, and also the
    keychain test's fixture calls (`SecItemImport` of SEC1, `SecItemUpdate`).
    S6 creates the crate, and S3 rebases onto it.
  - In the same PR, `AGENTS.md`'s Rust core row lists the crate, and the
    `unsafe` rule names it and the modules that already hold `unsafe` today: the
    shell's `permissions.rs` and `main.rs`; `steno-handover`'s
    `server/advertise.rs` and `upload/receiving_file.rs`; `steno-diarize`'s
    `coreml/binding.rs`; `steno-speech-sidecar`'s `lib.rs`; the test targets
    `steno-speech-sidecar/tests/isolation.rs` and `steno-speech/tests/frames.rs`.
- **D11 The handover's lost-complete-answer fix lands before the stable
  release.** New, decided by Nicolai on 2026-10-07. It is P2, with a fifth
  migration in both apps; the 14-day no-migration rule applies after the stable
  tag. Alternative: ship the fix after stable, with the migration inside the
  rollback window.
- **D12 The stability count is ten real meetings on the Mac and five on each
  Linux target to be listed.** New, confirmed 2026-10-07. Ten meetings cover a working
  week's calls on the Mac, the platform with every Swift user; five per Linux
  target cover its capture and session paths. The rules are in G2.
  Alternative: more meetings, or a number of days of daily use.
- **D13 A re-export never overwrites a note the user edited in the vault.** New,
  confirmed 2026-10-07. Nicolai: "Write beside." The export audit found that a
  re-export replaces the meeting's note whole, so an edit made in Obsidian is
  lost. If a note changed since Steno wrote it, Steno leaves it alone, writes
  the new version beside it as `<name> (Steno <date>).md`, and shows a warning
  (P30). Carrying the edited note's `[x]` task ticks over into the new version is
  not part of it. Alternative, not taken: Steno owns the note and the README
  says edits belong in Steno.

## What blocks the stable release

A row closes when its PR merges and its item leaves "Open after the port". Rows
with a PR number have one; the Rust plan's Progress table says whether it is
open or merged. The owner is the workstream that writes the package: audio;
capture and recovery (branches `wp-cap-*`); pipeline, store and export
(`wp-pse-*`); handover; Linux desktop.

**The handoff and the Mac (S1, S3 and S5 apply on every platform):**

| Item | Why it blocks | Package |
|---|---|---|
| The Rust app cannot download the CoreML Parakeet model | A fresh Mac install cannot transcribe | S1 |
| Whisper, Ultra and DE have no Rust engine | A Swift user who chose one gets Parakeet v3 after an unannounced 2.6 GB download | S1 |
| Downloads inside the pipeline (the speech models, the diarizer's models, the sidecar's install) | They fail silently or stall a meeting | S1 |
| Diarizer Settings wording and model licences | The wrong model is shown, and WeSpeaker's CC BY 4.0 licence requires attribution | S1 |
| Meeting detection and its prompt | Swift users start most recordings from the prompt; a missed meeting is a lost recording | S2 |
| Auto-stop after a call | Recordings run until someone stops them | S2 |
| Calendar lookup at recording start | Meeting titles and attendee names are lost | S3 |
| Permissions probe and the real `Permissions` | A fresh install, and every Swift user after the new id, can record silence | S3 |
| Update schedule | After the one-way handoff, fixes reach only users who check by hand | S4 |
| QR encoder | A new phone cannot pair | S4 |
| Bonjour re-publish after a network change, and the computer name | The phone loses the computer until Steno restarts | S5 |
| The new identifier, `SUPublicEDKey`, the import, the login item | The handoff itself | S6 |
| Build number, `v*` tags, the handoff item, `release.yml`'s concurrency | The release itself | S7 |

**No data lost, on every platform the release publishes (D3):**

| ID | What can be lost, and the fix | Owner |
|---|---|---|
| P1 | A recording: a power loss after `complete` must not roll back the meeting once the phone has deleted its copy. The intake commits its receipt, meeting and asset in one durable transaction, and the handover starts only after a durable checkpoint at launch that also restarts the WAL (#213) | handover (#213) |
| P2 | A recording, or a second meeting: a `complete` answer that never reaches the phone. A `handoverAdmission` table answers the retry "delivered", also for a recording whose meeting was deleted; the Rust migrator ignores later migrations and shows a dialog instead of panicking; the backfill runs on every open (`fix/handover-lost-complete-answer`, D11) | handover |
| P3 | A recording ended by a kill, a crash or a power loss: salvage the CAF at launch into a meeting that processes, instead of leaving it failed and unprocessed. The salvage writes the existing end reason `failed` (the interrupted row has none), so the Swift app still decodes the row during the rollback window (a new value in a stored enum column would break that); a recovered meeting is told apart additively, by a log line now and, if the UI needs it, a nullable column in a later add-only migration | capture and recovery (`wp-cap-*`, `fix/recording-recovery`) |
| P4 | A recording's stop: a `stop()` that waited behind a writer failure's or a device loss's finalise returns that recording, as Swift's actor did | capture and recovery (`wp-cap-*`) |
| P5 | The recording in progress: a save that outlasts the session's wait. Measure the save; an autostarted Linux app gets a systemd drop-in raising `TimeoutStopSec` from the generator's 5 s to 20 s, in the `.deb`, the AUR and Nix packages and written by the app for the AppImage (for the generator's `app-steno\x2ddesktop@autostart.service` under uwsm and Plasma, named after the Linux product name `steno-desktop`; GNOME starts autostart apps in its own `app-gnome-steno\x2ddesktop-<pid>.scope`, whose drop-in directory `app-gnome-steno\x2ddesktop-.scope.d` gets one too if its stop timeout is under 20 s); the save logs its duration at `warn`, so it shows under the default filter; on Windows, `ShutdownBlockReasonCreate` while recording | Linux desktop (with #220) |
| P6 | The recording in progress: systemd-oomd kills the app's cgroup with its sidecar. The sidecar moves into its own transient scope on Linux | Linux desktop |
| P7 | Every note: a people folder typed as `./People` or `.` in the Swift Settings makes each Rust delivery fail. `./People` becomes `People`; `.` becomes no people folder, as Swift wrote it | pipeline, store and export (`wp-pse-*`) |
| P8 | Every stored secret and the pairing on Omarchy: a multi-line secret corrupts its keyring. Every secret written to the Secret Service is one line (the PEM bundle base64-encoded, read back either way), with #221. At #221's first move, a key only `secrets.json` holds is copied; where both hold one, the file's API key wins and the Secret Service keeps its own `handover-identity`; after the move's mark, the Secret Service wins for every key | audio (with #221) |
| P9 | A failed meeting whose master exists: there is no "Process again". `ProcessingPipeline::reprocess` (landed, #228), a `meeting.processAgain` bridge method and its button, and `steno process --meeting <id>` (with `input` optional and exclusive of `--meeting`); a meeting refused for missing models stays queued and resumes once they install | pipeline, store and export (`wp-pse-*`) |
| P10 | A meeting's whole result: a diarizer or speaker-match failure fails the meeting. It merges without diarization instead | pipeline, store and export (`wp-pse-*`) |
| P11 | Speaker names confirmed while the meeting processes: the cleanup updates text by id, and `replace_transcript` keeps Confirmed assignments (calibration WP4) | pipeline, store and export (`wp-pse-*`) |
| P12 | A summary: `summarize` without a summarizer clears it. It keeps the existing one | pipeline, store and export (`wp-pse-*`) |
| P13 | A meeting stuck in a crash loop: a panic in `process()` marks the meeting failed, and a guard on resume attempts stops the loop | pipeline, store and export (`wp-pse-*`) (the panic wrap); audio (the crash-loop guard, #228) |
| P14 | Audio deleted by the retention sweep before its stamp is durable: the stamp commits durably first, and a meeting with no segments that is over 30 s long gets no stamp | pipeline, store and export (`wp-pse-*`) |
| P15 | Anything two processes write at once: one exclusive lock beside the database for the app's lifetime (#225). A second app instance that the single-instance guard does not hand over is refused with "Steno is already running"; the CLI's writing commands refuse while the app runs, and its read-only commands run without migrating, and refuse beside an older app. On the Mac the Rust app also refuses to start while the Swift Steno (`uno.schmid.steno.mac`) runs in the same login session (`NSRunningApplication`); a Swift app started after the Rust app is not kept out | capture and recovery (`wp-cap-*`, #225) |
| P16 | A meeting processed twice: the in-flight set is shared across pipeline reloads | pipeline, store and export (`wp-pse-*`) |
| P17 | A local recording's folder: a failed enqueue saves the asset row, so the folder is not orphaned; a panic in the session's rebuild ends the recording saved, as a lost device | capture and recovery (`wp-cap-*`) |
| P18 | A recording that silently stopped: the recorder subscribes to session failures | capture and recovery (`wp-cap-*`) |
| P19 | The mic lane when the input device goes away: the mic falls back to the default input mid-recording on every platform (Core Audio, PipeWire and WASAPI in #222), also when the chosen device is connected but does not open, and the recording returns to it once it is back and opens; one that does not open (at the start, or while it settles on its way back) is asked for again only at the next rebuild, when a default device moves or a device in use goes | audio (#222) |
| P20 | A recording that fills the disk: a free-space check before and during recording, with a warning. Linux and Windows refuse a start and stop the recording, saved, above a floor; the Mac half stays open until #236: `statvfs` leaves out APFS's purgeable space, so there a low reading only warns, and a disk that is truly full can fail the save, which P3 (#233) and P17 recover; the floor follows once `steno-macos` reads `NSURLVolumeAvailableCapacityForImportantUsageKey` | capture and recovery (`wp-cap-*`, #230; the Mac half #236) |
| P21 | The unsynced tail of a recording: periodic `sync_data` on the master | capture and recovery (`wp-cap-*`) |
| P22 | A lane that stopped delivering: a stall watchdog, and a recovery when the audio service restarts (`ServiceRestarted`) | capture and recovery (`wp-cap-*`) |
| P23 | Audio the relay dropped: a warning at stop and a log line; no stored count, since a column would need a migration | capture and recovery (`wp-cap-*`) |
| P24 | A transcript cut short by a sidecar shorter than its master: the sidecar's duration is checked against the master's | audio (#228) |
| P25 | A recording or a processing run stopped by an update: updates wait while either runs | Linux desktop |
| P26 | A person page: a case-only rename of a person loses the page on a case-insensitive disk | pipeline, store and export (`wp-pse-*`) |
| P27 | Notes written at once to one vault: deliveries are serialised per vault | pipeline, store and export (`wp-pse-*`) |
| P28 | A note never written: a delivery left Pending is resumed at launch. Amended 2026-10-08 under D3: a Failed delivery is retried at launch too, at most once a day (by its `lastAttemptAt`); after three launch retries in a row that did not deliver every row the launch stops retrying it and the meeting's export line says "Export to <destination> keeps failing: <reason>" until Export again resets the count. The count lives in `export-retries.json` in the support directory, which the Swift app ignores, so no migration | pipeline, store and export (`wp-pse-*`) |
| P29 | A note on Windows: names that Windows reserves (`CON`, `NUL`, ...) are escaped | pipeline, store and export (`wp-pse-*`) |
| P30 | A note the user edited: a re-export writes the new version beside it as `<name> (Steno <date>).md` and warns, leaving the edited note alone; task ticks are not carried over (D13) | pipeline, store and export (`wp-pse-*`) |
| P31 | Settings the other app wrote: a settings save upserts and keeps keys it does not know | pipeline, store and export (`wp-pse-*`) |
| P32 | Preferences, the panel anchor and the Codex sign-in: each is written atomically, and the Codex `auth.json` is synced | pipeline, store and export (`wp-pse-*`) |
| P33 | Another phone's upload: the first announce's discard, and a re-announce whose hash differs, leave other devices' files alone; an old device's 409 hands over cleanly (with P2) | handover (#219, #212) |
| P34 | Recordings on the phone: the mobile queue index rebuilds after a failed load, and recorder files left by the audio module are found again (#223) | handover (#223) |
| P35 | Pairings: the identity-fingerprint guard gets a macOS test and its Swift mirror (#213 makes the pairing writes durable and the intake one transaction) | handover |
| P36 | Secrets and files on Windows: credentials persist, and renames are durable | handover |
| P37 | A recording through a cancelled logout: the save that a logout started is undone cleanly when the logout is cancelled (#220) | Linux desktop (#220) |
| P38 | Evidence of a crash: a panic that unwinds leaves no report on the Mac and nothing where stderr goes nowhere. The app's and the sidecar's panic hooks write one `crash-<UTC>.log` file per panic in the support directory, with the message, the location and the backtrace; the newest 20 are kept | capture and recovery (`wp-cap-*`) |

**The final audio path (D9), on every platform:**

| ID | Package | Owner |
|---|---|---|
| A1 | A streamed decoder and mixdown: CAF and WAV decoded and mixed in bounded chunks, so a two-hour two-channel master never sits in memory whole | audio (#228) |
| A2 | The CoreML backend on the shared chunker, merge and decoder settings | audio |
| A3 | The diarizer on `ModelStore`, and its inference in the speech sidecar, so a crash in ONNX Runtime ends the child, not the app (invariant 4) | audio |
| A4 | PipeWire: `stop()` bounded, the own output and the default move settled (#214); `start`'s first cycle and the latencies measured on Nicolai's hardware | audio (#214) |
| A5 | One speech engine per reload | #218, merged |
| A6 | Devices that will not run at 48 kHz, and a headset's switch mid-call | #198 |
| A7 | The Linux input device list and the device UID fallback | #222 |
| A8 | Meeting detection on Linux, over PipeWire's streams | #222 |
| A9 | The final choices proven: the AAC priming trimmed; the resampler's sweep and speech tests; the 2 ms lag pinned; call mode without an output client checked on the Mac | audio |

**Per Linux target, blocking that target's listing (D6):**

| ID | What | Targets |
|---|---|---|
| X1 | Session end under Hyprland: the SIGTERM path under `uwsm`, the lost display (#220), logind's delay; the README tells users to start Steno from the launcher or with `uwsm-app` | Omarchy, NixOS with Hyprland |
| X2 | Panels under Hyprland: a `GDK_BACKEND` list keeps the panels on XWayland; the panels get distinct titles ("Steno bubble", "Steno prompt"); the README and the AUR package ship the Lua window rules (`float`, `pin`, `no_initial_focus`) | Omarchy, NixOS with Hyprland |
| X3 | The tray: every tray action is in its menu, since a left click cannot be relied on; on GNOME without the AppIndicator extension there is no tray, and closing the main window quits and saves | all |
| X4 | The handover behind a firewall: a fixed default port on Linux (configurable, `0` as the fallback with a warning); a `ufw` profile in the AUR package; the NixOS module opens the port | Omarchy, NixOS |
| X5 | Packaged installs: `STENO_DISTRIBUTION=aur` or `=nix`, read at build time (Nix) or from the environment (the AUR wrapper), turns the in-app updater off and Settings says updates come from the package manager; the autostart entry names a stable path (`/usr/bin/steno-desktop`, or the Nix profile's), never `current_exe()` or a store path; with `STENO_LOGIN_ITEM=managed`, which the NixOS module sets, the app leaves launch at login to the system | Omarchy, NixOS |
| X6 | The AUR package `steno-desktop-bin` | Omarchy |
| X7 | The flake's Linux package and NixOS module | NixOS |
| X8 | The Linux gate on each target | each |

**Also landing before the first candidate,** losing nothing but changing the code
the rehearsals run: #215 (the folder note names the platform, merged), #217 (no
title-bar band off the Mac, merged), #221 (Linux secrets in the Secret Service,
with P8).

## What follows

These lose no data and block nothing. Each keeps or gets an owner line in "Open
after the port".

- The tray's badge for pending speaker reviews; the main window shows them.
- The menu bar's queue and five recent meetings.
- The macOS Record and Find Meetings menu items; the page answers ⌘⇧R and ⌘F.
- The clip player.
- Layer-shell panels on wlroots, Hyprland and Plasma (X2's rules cover them
  until then).
- The speech settings without a Settings row, which
  `.plans/2026-10-07-speech-settings-ui.md` plans; the untested `steno-llm`
  cleanup paths.

## Work packages

Each package lands in one or more pull requests off `main`, reviewed and merged
by merge commit; a pull request may close several rows (#220: P5, P37 and X1; #222: A7, A8 and P19).
Steps marked **Nicolai** need him: secrets, settings on GitHub, his machines
and the phone. The letters: S for the Mac and the release,
A for the audio path, P for the other data-loss fixes, X for the Linux targets.
Every package is written in parallel except where a dependency is named:

- S3 on S6's `steno-macos` (D10); S4's packaged-install message on X5; A8 on S2's
  controller; X6 on X2, X4, X5 and P5; X7 on X4, X5 and P5; P5's save logging on
  P3's recoverable save; S1's queued refusal on P9's queue.

### S: the Mac and the release

- **S1 Speech models on the Mac** (`feat/rust-mac-speech-models`).
  - **The CoreML model.** Download Parakeet v3 from the Hugging Face repository
    FluidAudio reads (`FluidInference/parakeet-tdt-0.6b-v3-coreml`), pinned to
    commit `7dd20fe6b1` the way `PARAKEET_V3_FP32_REVISION` is pinned, through
    `steno_speech::ModelStore` into `fluidaudio/parakeet-tdt-0.6b-v3`. A new
    path on an existing host, so the PR adds it to invariant 3.
  - **The engine mapping.** A stored `whisperkit-large-v3-turbo`,
    `parakeet-ultra` or `parakeet-de` becomes `parakeet-v3` when the settings
    load, with a one-time notice ("Steno now transcribes with Parakeet v3"); the
    three rows leave Settings.
  - **No download inside the pipeline.** While any model the meeting needs is
    missing (the speech models in `prepare`, the diarizer's through `warm_up`,
    the sidecar's own install in `transcribe`), the meeting stays queued with
    "Download the speech model in Settings", and resumes once the models are
    installed (P9's queue).
  - **The diarizer row** describes the ONNX models: pyannote segmentation 3.0
    (MIT) and WeSpeaker ResNet34-LM, whose `licence` in
    `crates/steno-diarize/src/models.rs` changes from the wrong Apache-2.0 to
    CC BY 4.0 (VoxCeleb). Both notices show with attribution.
  - **Tests.** Unit tests for the manifest, the mapping and each refusal. On
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
  - The host's `Updater` over `updater.rs`:
    - a daily automatic check, as `SUScheduledCheckInterval` 86400 did;
    - stored automatic-check and automatic-download flags;
    - the last check time, under a file and key the PR names, in RFC 3339 UTC.

    A check is due at launch and hourly when that time is missing or more than
    24 hours old. The General section's Updates row reads them; a packaged
    install (X5) says "Updates come from your package manager".
  - A QR crate draws the pairing code into the pairing snapshot.
  - Tests: the schedule against a fake clock (due at launch, skipped, retried
    after a failure); the QR image decodes back to the pairing payload. R4
    proves the schedule on real releases.
  - In "Open after the port", S4 cuts the shell's four-gaps item down to the
    badge and the clip player.
- **S5 Handover on a changing network** (`fix/handover-republish`).
  - Re-register the Bonjour record when the interfaces change, on every
    platform.
  - On the Mac the shell sets `service_name` to the computer name
    (`whoami::devicename()`, `whoami` 2 as a new workspace dependency, which
    `cargo deny` must allow); Windows and Linux keep the host name.
  - Tests: a fake interface watcher triggers the re-registration.
    **Nicolai**, with a paired phone: switch the computer to another network and
    back; `dns-sd -B _steno._tcp` (or `avahi-browse -rt _steno._tcp` on Linux)
    shows the record each time, and the phone uploads without a restart.
- **S6 The new identifier and the import** (`feat/desktop-identifier`).
  - **Identifier.** `tauri.conf.json` sets `identifier` to
    `com.nicolaischmid.steno.desktop` (D5). `Info.plist` carries the Swift
    `SUPublicEDKey` (`RxaX7phoHvb7M0P4yaOC7zngDo+lqlOE6Iq89UtOuQI=`), inert in
    the Tauri app. `panel-anchor.json` moves into the support directory, read
    once from the desktop-id build's config directory. The desktop README's
    identifier paragraph follows. The PR creates `steno-macos`, makes the
    `AGENTS.md` change (D10), and adds `security-framework`, `plist` and the
    PKCS#12 crate to `[workspace.dependencies]`.
  - **The import, in two halves** (macOS only, in `steno-services`). It sets
    `steno.swiftImportRan` in `preferences.json` once both halves are done (or
    the second is skipped), and never runs again. Its sources (the defaults domain and the keychain) are
    traits. It is skipped under `STENO_SMOKE_SECONDS` and whenever `HOME` is not
    the account's home.
    - **At launch,** first in the shell's `setup`, before `Host::real` builds the
      graph: while `preferences.json` holds no onboarding flag, read the Swift
      domain explicitly (`/usr/bin/defaults export uno.schmid.steno.mac -`,
      parsed with the `plist` crate); copy `steno.onboardingCompleted` (not
      `steno.loginItemRegistered`, so the new identifier registers itself);
      copy Sparkle's `SUEnableAutomaticChecks` and
      `SUAutomaticallyUpdate` into S4's flags; drop the panel anchor (the panel
      opens at its default place). Whatever `preferences.json` holds, remove the
      Launch Agent a desktop-id build left behind, before the shell registers with
      `SMAppService`. If the keychain holds a Swift handover certificate (found by
      label, which does not prompt) and the import has not run, the graph is
      built with the import pending: `steno-services` reads no API key and starts
      no handover listener until the second half ends. A key alone does not make
      the import pending, since a desktop-id build files its key under the same
      service and account; the step reads a key only when it is the Swift one
      (labelled `Steno llm-api-key`, an attribute query that does not prompt).
    - **On an onboarding step,** shown first at that launch while the import is
      pending (without a Swift certificate there is no step, and the second half
      counts as done): the step says that
      macOS will ask for the login password once for each item it finds (at most
      twice) so the new Steno can read
      what the old one stored, and that the user should choose Always Allow.
      Then it reads the API key through `keyring` (one prompt; the item stays as
      it is, shared with the Swift app, which still reads it after a rollback),
      and exports the handover identity: the certificate by label,
      `SecIdentityCreateWithCertificate` through `security-framework`,
      `SecItemExport` as PKCS#12 through `steno-macos` (one prompt), decoded by a
      PKCS#12 crate (for example `p12-keystore`, which reads Apple's legacy
      encryption; `cargo deny` must allow it) into the PEM entry
      `handover-identity`, replacing a desktop-id identity. A denied read leaves
      the key empty, and Settings asks for it. A denied or failed export never
      mints an identity (D3): the handover listener stays off, and Settings'
      iPhone section says that Steno could not bring over this Mac's phone
      pairing, with Try again, which repeats the export and its prompt. Only
      when the user chooses Pair again there, which says that every phone must
      pair again, is a new identity minted: Pair again removes the paired phones
      and the recorded fingerprint, then mints through `HandoverIdentity::store`.
      S6 adds both as bridge methods, a waiting state for the listener with its
      fixtures, and a test that a refused read mints nothing. Then the pipeline starts, and the
      listener once the identity is in place. Skipping the step counts as a
      denied read and a denied export. The same rule holds outside the import:
      when an existing `handover-identity` cannot be read (a denied prompt, a
      locked keychain), `handover_listener` waits with Try again and never mints
      over it; it mints only where #221's guard allows (no identity, no recorded
      fingerprint and no paired phone), or when the user chooses Pair again.
  - **Login item** (D4). On macOS, `autostart.rs` uses `SMAppService.mainApp`.
    At the first launch after the handoff the new app registers its own
    identifier explicitly when the stored `launch_at_login` setting (shared
    database) is on. The Swift entry is
    left to macOS. If R3 shows that it stays as a second "Steno" in Login Items,
    the onboarding step and the release notes tell the user to remove it in
    System Settings > General > Login Items. If it launches a Swift copy, or
    blocks the new app's registration, a Swift bridge release unregisters it
    before installing (Release mechanics).
  - **Tests.**
    - A fixture plist covers the launch half, a missing key and an existing
      `preferences.json`. A second run is a no-op; with the import pending, the
      graph reads no key and binds no listener; the smoke skip is asserted.
    - A keychain test, behind `STENO_KEYCHAIN_TESTS=1` and run by the S6 author
      on Forge before the merge, opens a throwaway keychain by path with user
      interaction disabled, stores a committed fixture identity with the calls
      `IdentityKeychain.store` makes, runs the export against it through
      `kSecMatchSearchList` (with the keychain as
      `SecIdentityCreateWithCertificate`'s first argument and a PKCS#12
      passphrase), never touches the default keychain, and gets the fingerprint
      and `macID` a Swift test computes from the same fixture.
    - The prompts across apps and the old login item are R3's job.
- **S7 Release mechanics** (`ci/desktop-stable-release`). Everything under
  Release mechanics, `release.yml`'s deletion included.
  - Tests: a `*.test.sh` for every changed script, each `--handoff` failure
    among them, with `check-bundle.test.sh` stubbing `codesign` and `plutil`; a
    test that a pre-release does not bump the cask; `rust-ci.yml` runs every
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
  - The PR deletes the **WP9b.** item on the other Swift fixes, apart from call
    mode, which A9 deletes.
- **S8 Site and README** (`docs/stable-release-pages`). Written during the
  candidates and merged after R8 passes.
- **S9 Swift removal.** Cutover step 7, after the rollback window: the web app
  moves to `apps/web`, the Swift rows leave `AGENTS.md`, `apps/macos/scripts`
  goes (what the desktop workflow still calls moves to `apps/desktop/scripts`),
  and issue #74 is re-scoped or closed.

### A: the final audio path (D9)

Each lands before `0.11.0-rc.1`.

- **A1 A streamed decoder and mixdown.** Decode and mix every lane in bounded
  chunks, CAF and WAV included. The PR names two memory bounds: the decoder's,
  for its own test, and the soak's, for each of R5's commands (the app and its
  sidecar together). Decoded samples equal today's. The soak's bound is 6 GiB
  (#228); on atlas, `steno process` with the diarizer peaked at 4.26 GiB on a
  two-hour two-lane recording.
- **A2 CoreML on the shared chunker.** The CoreML backend moves onto the shared
  chunker, merge and decoder settings, settling the unticked WP4 integration
  notes. Parity: FLEURS and the Swift fixtures hold within today's tolerance.
- **A3 The diarizer on `ModelStore` and in the sidecar.** Its models come
  through `steno_speech::ModelStore` (resume, lock, mirror), and its inference
  runs in the speech sidecar through a request of its own. Parity: the
  diarization fixtures give the same clusters.
- **A4 PipeWire** (#214), plus `start`'s first cycle and the latencies measured
  on Nicolai's GNOME and Omarchy machines.
- **A5 One speech engine per reload** (#218, merged).
- **A6 Devices that will not run at 48 kHz** (#198).
- **A7 and A8 Linux devices and detection** (#222): the input device list, the
  device UID fallback and meeting detection over PipeWire's streams, behind S2's
  controller. Tests: table tests over a fake PipeWire node list (a Firefox or
  Chromium input stream raises the prompt, an output-only stream does not,
  Steno's own stream is ignored).
- **A9 The final choices proven.**
  - The AAC priming offset is trimmed from the container's edit list.
  - A 44.1 kHz sweep to 22 kHz, resampled, leaves less than −60 dB below
    8 kHz, and speech recorded at 48 kHz and resampled from 44.1 kHz gives the
    48 kHz path's word error rate within 0.1 points.
  - The 2 ms sidecar lag stays pinned by `tests/codec.rs`.
  - **Nicolai**, on his Mac in a GUI session with every permission granted, with
    the Rust app built from A9's branch and speakers, not headphones: start a
    call recording while no app plays audio, and play a short tone at 10 s.
    Pass when the tone's onset in the system channel and its residual in the
    echo-cancelled mic channel lie within 50 ms (if the cancellation hides the
    residual, a second take with `steno record --keep-raw-mic --out ~/a9-raw`
    from A9's branch keeps the raw mic as `mic.raw.caf`), and
    the tap's first callback comes within 100 ms of the capture's start. A9 adds
    an `info` line with that callback's offset from the capture's start, stored
    by the IOProc in an atomic and logged off the audio thread. Nicolai quits
    every Steno, the Swift app included, creates the log (`: > ~/a9.log`) and
    runs `open --env RUST_LOG=info --stderr ~/a9.log
    ~/Applications/Steno.app`, so Launch Services starts it under its own grants.
    If either bound is missed, the remedy becomes an A-package before the first
    candidate.

### P: no data lost (D3)

The table above names each package and its owner. Their tests:

- **P2.** Its tests include a store written by the Swift `v0.10.0-rc.2` intake (a
  phone meeting with no `handoverAdmission` row), opened by the Rust app, which
  answers a re-sent `complete` for that recording as the PR states, with no
  second meeting; the schema parity test covers v5; a v6 migration applied to a
  copy is ignored with a warning.
- **P3.** An integration test runs the recorder over a fake capture source in a
  child process with a fake speech engine and kills it (SIGKILL,
  `TerminateProcess` on Windows) before the first flush, after it, mid-recording,
  and while `stop()` is held at a gate, in `rust-ci.yml` on all three platforms.
  After a kill past the first flush, the next launch lists the meeting `queued`
  with `endReason` `failed` and no failure reason, with audio up to the last
  flush, and processes it. A kill before the first flush leaves state `failed`,
  not a missing meeting. Each flush is followed by
  `sync_data` (P21), asserted through a counting file.
- **P4.** The interleaving forced with a gate.
- **P5.** The save of a two-hour recording is measured on Nicolai's slowest
  Linux machine, and on Windows in the Windows gate; it passes under 10 s. The
  gates' logouts follow a recording of at least an hour.
- **P6.** In the Omarchy gate, while a meeting processes, `cat /proc/$(pidof -s
  steno-speech-sidecar)/cgroup` names a scope of its own, not the app's.
- **P8.** A test that no secret written to #221's fake service holds a newline.
- **P28.** A ready meeting whose delivery is `pending`, or `failed` more than
  a day before the launch or never attempted, is re-exported at launch, one
  meeting at a time; one that failed within the day, one delivered and a
  meeting that is not ready are not. A failed export is retried by one launch a
  day at most, is left with the "keeps failing" line after three launches in a
  row that did not deliver every row, and the next launch retries it once
  Export again resets the count. An `App::launch`
  test re-exports a `pending` row into a vault.
- **P30.** A note changed in the vault since Steno wrote it (its hash differs
  from the ledger's) gets the new version beside it as `<name> (Steno
  <date>).md` and a warning, and stays as it was; an unchanged note is replaced.
- **P9.** A failed meeting with its master processes again from the button and
  from `steno process --meeting <id>`; a meeting queued for missing models
  resumes after the install.

### X: the Linux targets (D6)

- **X1 to X5** are the rows above.
- **X6 The AUR package `steno-desktop-bin`** (`packaging/aur/`). The PKGBUILD
  extracts the release `.deb` (moving the binary and its sidecar to
  `/usr/lib/steno-desktop/`), verifies it against its `.asc` with
  `validpgpkeys` set to the release key
  (`048B527950E4F609B90E63495F8810A6E6D4DB46`), depends on `webkit2gtk-4.1`,
  `gtk3`, `glib2`, `libsoup3`, `libayatana-appindicator`, `pipewire` and
  `dbus`, and ships P5's drop-in, X4's `ufw` profile and a `/usr/bin` wrapper
  that sets X5's flag and the path the autostart entry names. The first stable
  push is by hand; then **Nicolai** adds
  `AUR_SSH_PRIVATE_KEY`, and `publish` pushes from the second stable release
  on. A candidate is installed with `makepkg -si` from `packaging/aur/`, with
  `pkgver` in pacman form (`0.11.0rc1`, which `vercmp` ranks below `0.11.0`).
- **X7 Nix on Linux.** `flake.nix` gains `packages.x86_64-linux.steno`, built
  from source with `rustPlatform` and `cargo-tauri.hook`, `wrapGAppsHook3`,
  `ORT_LIB_LOCATION` against nixpkgs' `onnxruntime` with
  `ORT_PREFER_DYNAMIC_LINK=1`, the sidecar staged by `stage-sidecar.sh`, the
  tray library's `dlopen` path patched, `STENO_DISTRIBUTION=nix`, and P5's
  drop-in, built from `self` with every hash in the tree, so any tag builds as
  it is. Without the module, the autostart entry names, as an absolute path, the
  first of `$HOME/.nix-profile/bin/steno-desktop` and
  `/etc/profiles/per-user/$USER/bin/steno-desktop` that resolves into the
  running package, else the bare `steno-desktop`; never a store path. Because this links a
  different ONNX Runtime build, the PR re-runs
  FLEURS against pyke's build and states both word error rates. It also gains
  `nixosModules.default` (`programs.steno.enable`): the package, a systemd user
  service, `steno.service`, started with the graphical session
  (`TimeoutStopSec=20s`, `STENO_LOGIN_ITEM=managed`) that runs the stable
  profile path the module picks, `/run/current-system/sw/bin/steno-desktop` for a system install or
  `/etc/profiles/per-user/<user>/bin/steno-desktop` for a per-user one, X4's
  firewall port, PipeWire on, a Secret
  Service provider (`services.gnome.gnome-keyring.enable` unless one is set),
  and an opt-in raise of logind's `InhibitDelayMaxSec`. The macOS output stays
  as it is. A candidate is built from its tag
  (`nix build github:NicolaiSchmid/steno/v0.11.0-rc.N#steno`).
- **X8 The Linux gates** (Rehearsal).

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

**Migrations add, never change.** From 0.11.0 on, a migration only adds tables
or columns (a new column has a default or allows null) and never alters, drops
or constrains an existing one, and never writes a new value into an existing
enum column (an older build's decoder would refuse the row). Every build since
0.11.0 carries P2's migrator, which ignores migrations it does not know, so each
still opens a later database, and the add-only rule lets it keep writing. A rollback to a desktop-id build from before P2
(`desktop-v0.1.0-rc.*`) is the one exception: it refuses a v5 database, so such
an install needs a newer build installed by hand (Rollback).

### The workflow after S7

- **Trigger and names.** The trigger becomes `tags: ['v*']`, and the
  `desktop-v*` trigger goes. The release title becomes "Steno <version>". The
  manifest base URL passed to `updater-manifest.sh`, and the tag in the
  summary's notes, become `releases/download/v<version>`.
- **macOS bundle job.**
  - `check-bundle.sh --signed --handoff <build>` fails unless all of these hold:
    - `CFBundleIdentifier` is `com.nicolaischmid.steno.desktop`;
    - `SUPublicEDKey` is the Swift key;
    - `CFBundleVersion` equals `<build>`;
    - the designated requirement names team `KQB68F43PW`;
    - `codesign --verify --deep --strict` passes.
  - On tag runs only, after "Notarise the disk image" (the EdDSA signature
    covers the stapled DMG's bytes), a "Handoff item" step signs:
    - it runs `generate_appcast` from the Sparkle 2.10.0 tarball (SHA-256
      pinned), with `--ed-key-file -` reading `SPARKLE_PRIVATE_KEY` (in this
      step's environment only), `--download-url-prefix
      .../releases/download/v<version>/`, `--maximum-deltas 0` and D8's rollout
      interval;
    - it fails unless the output carries `sparkle:edSignature`
      (`generate_appcast` skips signing without an error when the keys do not
      match);
    - it dry-runs `merge-appcast.py` against the current `appcast` branch.
  - The item goes into its own artifact, `sparkle-item`, outside the
    `steno-desktop-*` pattern that `assets` downloads.
  - Check secrets gains `SPARKLE_PRIVATE_KEY`, and the desktop README's secrets
    table lists it.
- **`publish`.**
  - A version with a hyphen publishes as today: `--prerelease --latest=false`.
  - A version without one:
    - `gh release edit "$TAG" --draft=false --prerelease=false --latest`;
    - upload the `appcast` branch's `appcast.xml`
      (`git fetch origin appcast && git show FETCH_HEAD:appcast.xml`) as the
      release's `appcast.xml`;
    - bump the cask with `apps/macos/scripts/bump-homebrew-cask.sh` once
      `HOMEBREW_TAP_TOKEN` exists (D7);
    - push the AUR bump once `AUR_SSH_PRIVATE_KEY` exists (X6);
    - write the Nix flake lines (the macOS DMG's hash) to the summary;
    - output whether the branch already has an item without a channel.
  - The lane releases stay pre-releases with `--latest=false`.
- **`handoff`.** Stable tags only, behind the GitHub environment `appcast`,
  whose required reviewer is Nicolai. Its job-level `if` reads `publish`'s output
  (with `plan` in `needs`), so it is skipped without asking once the branch has
  its item. After the approval it:
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
  - Every `0.11.0-rc.N` and `0.11.0` carry the desktop-id paragraph: such an
    install converts to the new identifier, asks once more for its permissions
    and its keychain items (choose Always Allow), and a Mac that ran both apps
    from two paths keeps two copies of one app, of which the one outside
    `/Applications` can be deleted.
  - `0.11.0` carries the handoff paragraph:
    - Swift users receive this release as an update;
    - macOS asks once more for microphone, system audio and calendar, and for the
      login keychain password once per stored secret (choose Always Allow);
    - an old "Steno" entry in Login Items may need removing (S6);
    - phones stay paired; if macOS's export prompt is denied, Steno keeps the
      phones waiting and offers Try again in Settings > Phones, and phones pair
      again only where D5 says so or the user chooses Pair again;
    - the optional mixdown is now WAV;
    - Whisper, Ultra and DE now transcribe with Parakeet v3;
    - Homebrew users whose app updated itself can run
      `brew upgrade --greedy --cask nicolaischmid/tap/steno`;
    - the install lines for Omarchy (`yay -S steno-desktop-bin`) and NixOS;
    - the Linux known issues: the KDE Plasma logout until Plasma calls the
      portal monitor, the whole default sink as the system lane, the WebKitGTK
      descriptor leak on very long sessions, no tray on GNOME without the
      AppIndicator extension, and a Steno started outside `uwsm` on Hyprland.
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
and also:

- `url ".../releases/download/v#{version}/Steno_#{version}_aarch64.dmg"`;
- `livecheck` for stable versions only, keeping the anchor:
  `/^v?(\d+(?:\.\d+)+)$/`;
- `zap` adds `~/Library/Application Support/com.nicolaischmid.steno.desktop`,
  `~/Library/WebKit/com.nicolaischmid.steno.desktop`,
  `~/Library/Caches/com.nicolaischmid.steno.desktop` and
  `~/Library/Preferences/com.nicolaischmid.steno.plist`, and keeps the `uno.*`
  entries;
- the caveat says Steno updates itself.

`auto_updates true` stays.

The AUR package is X6; its first push is **Nicolai**'s, in G3. In S7,
`flake.nix`'s macOS output gets the new URL, checks for
`Contents/MacOS/steno-desktop` and `bundleId = "com.nicolaischmid.steno.desktop"`, and
its UPDATES text names Settings' automatic-check switch; X7 adds the Linux
outputs. The flake bump stays a manual PR made from the job summary.

## The Sparkle handoff

### The handoff item

One item without a channel, served from the `appcast` branch (for
`v0.9.0-rc.2` and later) and as the `appcast.xml` asset of every stable release
(for `v0.9.0-rc.1`, through `releases/latest`). Its fields:

- `sparkle:version` is the commit count, above 542;
- `sparkle:shortVersionString` is `0.11.0`;
- `minimumSystemVersion` is 15.0, and `hardwareRequirements` is `arm64`;
- the enclosure is `.../releases/download/v0.11.0/Steno_0.11.0_aarch64.dmg`,
  with its length and `sparkle:edSignature`;
- the `pubDate` is the approval's.

The Swift items stay below it. Nix installs see it but cannot install from the
read-only store.

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
that path holds `com.nicolaischmid.steno.desktop`.

### First launch after the handoff

| What | What happens | Proven by |
|---|---|---|
| Meetings, audio, models | The same support directory, same schema | R3 |
| Preferences | Read from `uno.schmid.steno.mac` at launch (S6) | R3 |
| TCC grants | None carry over; onboarding opens on its permissions page (S3), and the user grants each once | R3 |
| API key | Read once on the import step, one prompt with the login password; Always Allow keeps the new app on the shared item | R3 |
| Handover identity | Exported once on the import step, one prompt with the login password, stored as the new app's PEM entry; the listener starts only after it; the Swift item stays | R3: the phone uploads without pairing again |
| Codex sign-in | The Codex CLI's own `auth.json`, untouched | R3 |
| Login item | The new app registers itself when `launch_at_login` is on; the Swift entry is left to macOS (S6) | R3 |
| Updates | The Tauri updater reads `desktop-stable` daily (S4) | R4 |

## Landing page and README

S8 is merged after R8 passes.

- **`apps/site/src/lib/site.ts`.** `download` stays `releases/latest`.

  | Platform | Note | Listed as released |
  |---|---|---|
  | `mac` | "Apple Silicon · notarised" | yes, as today |
  | `linux` | "GNOME (.deb · AppImage) · Omarchy (AUR) · NixOS (flake)", naming only the targets whose gates passed | once one Linux gate passes |
  | `win` | "x64 · installer not code-signed" | once its gate passes |

  The download button needs no change. The closing CTA's Linux tile shows the
  install line per listed target (the `.deb`, `yay -S steno-desktop-bin`,
  `programs.steno.enable`).
- **`how-its-built.tsx`.** The subhead loses "is moving". The closing paragraph:
  "Every desktop runs the same Rust app. On the Mac it replaced the original
  Swift app as an ordinary update, with your meetings, settings and paired phone
  where you left them."
- **`open-source.tsx`.** "Swift Mac app · Rust core · Tauri shell" becomes "Rust
  core · Tauri shell".
- **`page.tsx`.** The JSON-LD `operatingSystem` lists the released platforms.
- **README.**
  - The status banner says stable and links the release.
  - Badges: the release badge drops `include_prereleases`, Rust CI replaces
    Swift CI, and the platform badge names the released desktops.
  - Install, one block per OS:
    - **macOS:** Homebrew, the DMG and Nix; updates come through the app.
    - **Windows:** the MSI or the `-setup.exe`, not code-signed, so SmartScreen
      asks first; the hash against `SHA256SUMS`.
    - **GNOME:** the `.deb` or the AppImage, verified with the `gpg --verify` and
      `sha256sum --check` lines from the desktop README (whose key URL moves to
      `v<version>`); the AppIndicator extension for the tray.
    - **Omarchy:** `yay -S steno-desktop-bin`; `sudo ufw allow Steno`; start Steno
      from the launcher; the tray icon sits in the bar's drawer, and a right
      click opens its menu; the Lua window rules for the panels (X2).
    - **NixOS:** the flake input and `programs.steno.enable = true`.
  - "Homebrew details" says the tap follows stable releases (D7). "For
    developers" puts the Rust workspace first; the Swift lines go in S9.
- **`apps/desktop/README.md`.**
  - Release, "Cutting a release", "When a run fails", "Publishing, on a tag" and
    the Layout table use `v<version>` and the full release.
  - "A bad release" covers the first stable release (Rollback).
  - "Not here yet" loses what the packages close and marks the clip player as
    following.
- **Comments.** The "never latest" comments in `updater.rs` and at the top of
  `desktop-release.yml` go.

## Rehearsal

Nothing reaches Swift users until G2 has passed: R1 to R6, the dogfood, the
Linux gates for the targets to be listed and the stability count. The stable tag
then rebuilds the same code with only the version and the build number changed.
R7 checks that build before Nicolai approves the handoff item, and R8 checks it
through the real feed while the phased rollout has reached only its first group.

The steps run in three places.

**Forge** (`ssh forge`), which has no `sudo`:

- work goes under `~/steno-handoff/`, never `~/steno-calibration/`, and no app
  goes into `/Applications`;
- `ditto` copies only into a path that does not exist; to replace a bundle,
  remove the old one first;
- assets and artifacts are fetched on atlas and copied over;
- before R2 and before R7, `defaults export uno.schmid.steno.mac
  ~/steno-handoff/defaults-before.plist`; after each, `defaults import` restores
  it and `~/Library/Caches/uno.schmid.steno.mac/org.sparkle-project.Sparkle` is
  removed, since `sparkle-cli` writes the bundle id's real domain.

**A fresh account on Nicolai's Mac,** a new macOS user per step that says so; R4
and R6 reuse R3's:

- every Steno build there goes into `~/Applications/Steno.app` and is opened by
  path; nothing touches `/Applications`;
- no other copy of the Swift app exists in the account, and the DMG is ejected
  after copying;
- the account is an administrator (`sfltool dumpbtm` needs `sudo`), and
  Nicolai's daily Steno is quit in his own account during a step;
- before every logout in a login-item check, Steno is quit from its menu and
  "Reopen windows when logging back in" is unticked, so only login items start
  it;
- a modal dialog that the step does not list fails it; the "Background Items
  Added" notification does not.

**Nicolai's Linux machines** for the Linux gates.

**The local feed** (R2, R3, the dogfood) holds the candidate's `sparkle-item` and
DMG, with the enclosure rewritten to `http://localhost:8765/<dmg>`. The EdDSA
signature covers the file, the Swift app does not set `SURequireSignedFeed`, and
ATS refuses bare IP addresses. It is served with `python3 -m http.server 8765
--bind 127.0.0.1` in the account that runs the step, and `curl -fsS
http://localhost:8765/appcast.xml` must answer first. The server's access log
(`GET /appcast.xml`, and `GET /<dmg>` for an install) proves the feed was read.

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
<plist>`. Each plist has:

- a unique `Label`;
- absolute `ProgramArguments` with the case's flags and `--user-agent-name
  steno-rehearsal`;
- `RunAtLoad` true and no `KeepAlive`;
- `StandardOutPath` and `StandardErrorPath` under `~/steno-handoff/logs/`.

Poll `launchctl print gui/$(id -u)/<label>` until `state = not running`, at most
ten minutes, read `last exit code`, then `bootout`. Cases run one at a time: the
next starts only when `launchctl list | grep uno.schmid.steno.mac-sparkle` is
empty. Swift release candidates read `beta`, so the runs pass `--channels beta`,
except case e.

### The steps

- **R1 Desktop-id conversion** (**Nicolai**, a fresh account on the Mac, and a
  fresh account on his GNOME machine).
  1. Install `desktop-v0.1.0-rc.2` (on Linux, the AppImage). Grant the
     permissions, set an API key, pair the phone, turn on launch at login, move a
     floating panel, and record one short call (it cannot be transcribed yet on
     the Mac: that build has no CoreML download, so the meeting fails or waits).
  2. When the candidate is on `desktop-beta`, check for updates and install.
  3. Download the speech models in Settings; the first call resumes, or Process
     again (P9) runs it; record a second call.

  Pass when:
  - the app relaunches as `com.nicolaischmid.steno.desktop`, and launch at login starts
    it once; `~/Library/LaunchAgents` holds no Steno plist, and with launch at
    login off a login starts nothing;
  - the panel opens where it was moved (the anchor read from the old
    directory);
  - on the Mac, no import step appears; onboarding opens on its permissions page
    at the first launch and each TCC prompt appears once; one keychain prompt
    with the login password appears per item the desktop-id
    build created (`handover-identity` and the API key, both at launch), and the
    `handover-identity` prompt appears again after Try again. It is first
    denied: no identity is minted (`lsof -nP -a -c steno-desktop -iTCP -sTCP:LISTEN` in this account shows no listener, and Settings' iPhone section
    offers Try again); Try again brings it back, and with Always Allow none
    appears again;
  - the phone uploads without pairing again;
  - both calls are listed, transcribed and summarised.
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

  Case a's checks, the proof on the real bundles that Sparkle hands over across
  bundle ids:
  - the bundle at the Swift app's path now has `CFBundleIdentifier`
    `com.nicolaischmid.steno.desktop`, `CFBundleExecutable` `steno-desktop`, the
    candidate's build number and the Swift `SUPublicEDKey`;
  - `codesign --verify --deep --strict` passes, and `codesign -d -r-` names team
    `KQB68F43PW`;
  - `spctl -a -t exec -vv` says "Notarized Developer ID".
- **R3 The real handoff** (**Nicolai**, a fresh account; the cutover plan's
  tests 1 to 3). The phone keeps one pairing, so pairing it here unpairs his
  daily install until he pairs it there again.
  1. Install `v0.10.0-rc.2`. Grant every permission, set an API key, pair the
     phone, record and process a meeting, and turn on launch at login. Save
     `sudo sfltool dumpbtm` as `btm-before.txt`.
  2. Quit. Run `defaults write uno.schmid.steno.mac SUFeedURL
     http://localhost:8765/appcast.xml` (Sparkle 2.10 honours it in release
     builds), and relaunch.
  3. Check for Updates and install.
  4. Pass when:
     - the app comes back as `com.nicolaischmid.steno.desktop`, and the meeting is
       listed;
     - the import step comes first and shows its note; then two keychain
       prompts appear, each asking for the login password, one for the API key
       and one to export the handover key, which appears again after Try again
       (record each prompt's wording). Always Allow is chosen for the key. The export prompt is first
       denied: no identity is minted (`security find-generic-password -s
       uno.schmid.steno.mac -a handover-identity` finds nothing new, and
       `lsof -nP -a -c steno-desktop -iTCP -sTCP:LISTEN` in this account shows no listener), and Settings' iPhone section offers Try again. Try again brings the prompt back, and Always Allow is chosen;
     - no keychain prompt appears before the step, and until the step ends
       `lsof -nP -a -c steno-desktop -iTCP -sTCP:LISTEN` in this account shows no listener;
     - onboarding then opens on its permissions page, and each of microphone,
       system audio and calendar prompts once;
     - after that, a summary runs with the stored key and no prompt, and the
       phone uploads without pairing again;
     - a call records both lanes;
     - Settings shows the CoreML Parakeet as installed.

     Any other modal dialog fails R3.
  5. **The old login item.** Save `sudo sfltool dumpbtm` again as
     `btm-after.txt`. Log out and in: Steno starts once. Turn launch at login off
     in the new app, save `sudo sfltool dumpbtm` as `btm-off.txt`, log out and
     in: if Steno still starts, the old entry launches the new app. Record what
     Login Items shows; S6's handling follows from it. End the step with launch
     at login on again.
  6. Run `defaults delete uno.schmid.steno.mac SUFeedURL`.
- **R4 Updates after the handoff** (**Nicolai**, R3's account; the cutover
  plan's test 4).
  1. Publish the next candidate, which changes only the version.
  2. Quit. Set S4's stored last check time to 25 hours ago in the file the S4
     PR names, and relaunch.

  Pass when:
  - the scheduled check finds the new candidate and offers or installs it;
  - after the relaunch, `ps -o command= -p $(pgrep -u "$(id -u)" -x
    steno-desktop)` shows `~/Applications/Steno.app/Contents/MacOS/steno-desktop`
    and About shows the new version;
  - no keychain or TCC prompt appears again;
  - with the speech setting on the ONNX engine for one meeting,
    `steno-speech-sidecar` starts from the same bundle (switch it back).
- **R5 Fresh install** (**Nicolai**, a fresh account; the soak on Forge; the
  cutover plan's test 5).
  1. With networking off, open the downloaded, quarantined DMG; `spctl -a -t
     open --context context:primary-signature -vv` accepts it, and the app
     opens. Turn networking back on.
  2. Onboarding asks for each permission.
  3. Settings downloads the CoreML Parakeet and the diarizer models, with
     progress.
  4. A five-minute call is transcribed, diarized and exported.
  5. On a candidate built after the last P package, including any the audit
     adds later (G2). On Forge, with
     `HOME=~/steno-handoff/soak-home` (the models copied into its
     `Library/Application Support/Steno/Models/`, no API key, no destination),
     with `steno` and `steno-speech-sidecar` built from the tag into one
     directory and the default four ONNX threads:
     - `steno process mic.wav --system-lane system.wav --source mac-call
       --engine parakeet-v3` on a two-hour two-lane FLEURS recording
       (generated, never committed);
     - `steno dev bakeoff` on a two-hour mono 44.1 kHz m4a and a two-hour
       two-channel 48 kHz Float32 CAF without sidecars, each in its own
       directory.

     Pass: for each command, the peak of the resident memory of `steno` plus its
     children (each second, the sum of `ps -o rss=` over `steno` and `pgrep -P
     <steno>`) stays under A1's soak bound, and the transcripts are complete.
- **R6 Rollback drill** (**Nicolai**, R3's account; the database is v5 by now).
  1. Save the Tauri app's requirement without its prefix
     (`mkdir -p ~/steno-r6 && codesign -d -r- ~/Applications/Steno.app 2>/dev/null | sed -n 's/^designated => //p' > ~/steno-r6/tauri-dr.txt`)
     and keep the app as an archive, so no second copy is registered
     (`ditto -c -k --keepParent ~/Applications/Steno.app ~/steno-r6/Steno.zip`).
  2. **To Swift.** Quit Steno, remove `~/Applications/Steno.app`, and `ditto` the
     `v0.10.0-rc.2` app from its DMG into its place. Pass when:
     - it opens the v5 database with no error and lists every meeting,
       including the ones the Tauri app recorded;
     - it records with its own TCC grants and reads the API key with no prompt;
     - the phone uploads a new recording to it;
     - with launch at login on, as R3 left it, Login Items shows what R3
       recorded, and Steno starts once at login.
  3. **Back to Tauri.** Quit, remove the bundle, and `ditto -x -k
     ~/steno-r6/Steno.zip ~/Applications/`. Pass when `codesign --verify -R
     "=$(cat ~/steno-r6/tauri-dr.txt)" ~/Applications/Steno.app` passes, it lists the meeting the Swift app
     received in step 2, and the phone uploads a new recording without pairing.
  4. **To the previous candidate.** Install the previous candidate's DMG the same
     way. Pass when it opens the database, lists every meeting, and its own
     updater then offers the current candidate.
  5. **To `desktop-v0.1.0-rc.2`,** with Steno quit, on a copy of the database
     (with its `-wal`) in a scratch `HOME`, each build run as `HOME=<scratch>
     <app>/Contents/MacOS/steno-desktop` from Terminal: it refuses the v5 database and exits, the file is unchanged
     (`shasum` before and after), and installing a current build by hand opens
     it again. This is the documented manual-reinstall case.
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

  Pass: the meetings count towards the stability count (G2); one phone upload
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
    default. Pass: each relaunches as `com.nicolaischmid.steno.desktop` 0.11.0, lists
    the meeting, and shows only R3's prompts;
  - a 0.11.0 install's Check for Updates reports up to date against
    `desktop-stable` (HTTP 200);
  - `gh api repos/NicolaiSchmid/steno/releases/latest --jq .tag_name` answers
    `v0.11.0`;
  - the branch's only item without a channel is 0.11.0, with the rollout
    interval and the approval's `pubDate` in the Swift items' date form;
  - **Homebrew** (after Nicolai's tap commit, his own account):
    1. pull the tap
       (`git -C "$(brew --repository nicolaischmid/tap)" pull --ff-only`, or
       `brew tap nicolaischmid/tap`);
    2. `HOMEBREW_NO_AUTO_UPDATE=1 brew fetch --cask nicolaischmid/tap/steno`
       downloads 0.11.0 and verifies it;
    3. the DMG in `brew --cache --cask nicolaischmid/tap/steno` holds
       `Contents/MacOS/steno-desktop`;
  - **Nix on the Mac:** on Forge or Nicolai's Mac, the flake bump branch builds,
    and its `result/Applications/Steno.app/Contents/Info.plist` names
    `steno-desktop`, `com.nicolaischmid.steno.desktop` and 0.11.0;
  - **Linux:** on the GNOME, Omarchy and NixOS machines, each gate's step 0,
    from the last candidate to `v0.11.0`, and one recording.

### The Linux gates

Each runs on Nicolai's own machine, in a new user account, before the site lists
the target, from `0.11.0-rc.2` on (`rc.1` has no previous candidate). Step 0 on
each target installs the previous candidate (`rc.<N-1>`), opens the firewall
where there is one, pairs the phone, records a one-minute meeting and turns on
launch at login (on NixOS the module's `steno.service` does), then upgrades to
the
candidate under test: `sudo apt install ./<new>.deb` on GNOME; `makepkg -si` in
`packaging/aur/` at the new tag on Omarchy; the flake input moved to the new
tag, `nixos-rebuild switch` and `sudo nix-collect-garbage -d` on NixOS. Step 0
passes when the meeting and the pairing are kept and, after a logout and login,
Steno starts once, as the new version. Each target's step 1 sets up its step 0;
steps 2 onward run on the candidate under test. On `v0.11.0`, R8 repeats step 0
from the last candidate and one recording.
The status query used throughout (`sqlite3`, or `nix-shell -p sqlite` on NixOS):

```sh
q() { sqlite3 ~/.local/share/Steno/steno.sqlite \
  "select datetime(startedAt), state, round(duration,1), endReason, failureReason from meeting order by startedAt desc limit 3"; }
```

Good: the newest meeting is `queued` or later, with a duration near the time
recorded; after a logout, a reboot or a compositor exit, its `endReason` is
`quit` (the save at session end), never `failed` (P3's salvage). Bad: state
`recording` with 0.0 s before a relaunch, or state `failed` "Recording was
interrupted" after one. On the GNOME machine,
`systemd-coredump` is installed before the gate, so crashes show in
`coredumpctl list`.

- **GNOME** (Ubuntu 24.04 or Debian 13, named in the result).
  1. Step 0 verifies each `.deb` first with the README's `gpg --verify` and
     `sha256sum --check`.
  2. Onboarding shows no permission as missing; Settings downloads the speech
     model with progress; Settings > Recording lists the input devices (A7).
  3. The phone, paired in step 0, uploads a recording.
  4. Join a Google Meet call in Firefox: the detection prompt appears (A8);
     record it, with the floating panel on top; stop; the meeting has a
     microphone lane and a system lane, and is transcribed, diarized and
     exported.
  5. With launch at login on since step 0, log out and in; the last path
     component of `cat
     /proc/$(pidof -s steno-desktop)/cgroup` names the unit that holds Steno, and
     `systemctl --user show '<that unit>' -p TimeoutStopUSec` (single-quoted) is
     at least 20 s (P5).
  6. Record for at least an hour, then log out: after logging back in, `q` shows
     the meeting with its full length and `quit` as `endReason`, it processes,
     and `journalctl --user -b 0 --since '<logout time>' | grep -i steno` shows
     the save's logged duration before the app exits, and neither "timed out.
     Killing" nor "Failed with result 'timeout'" (P5). Record, then reboot: the same, with
     `journalctl --user -b -1`. Record, then `kill -9` the app: at the next launch the
     meeting is recovered with `failed` as `endReason` and processes (P3).
  7. Quit the `.deb`'s Steno; the AppImage starts (`pgrep -a steno-desktop`
     shows only its path) and finds the same meetings.
- **Omarchy** (Omarchy 4, Arch with Hyprland on Wayland).
  1. Before step 0: import the release key (`gpg --recv-keys
     048B527950E4F609B90E63495F8810A6E6D4DB46`), so the PKGBUILD verifies each
     `.deb`'s signature; add the README's Lua window rules to `~/.config/hypr/`
     and reload Hyprland. In step 0, `sudo ufw allow Steno` runs right after the
     install and before the pairing. From `v0.11.0` on, step 0 installs with
     `yay -S steno-desktop-bin`.
  2. Start Steno from the Omarchy launcher. `cat /proc/$(pidof -s
     steno-desktop)/cgroup` names an `app-…scope` (not Hyprland's unit). The
     tray icon is in the bar's drawer; a right click opens the menu, and every
     entry works (X3). While a meeting processes, the sidecar's cgroup is a
     scope of its own (P6).
  3. Start a recording: `hyprctl clients -j | jq
     '.[]|select(.class=="steno-desktop")|{title,xwayland,floating,pinned}'`
     shows the bubble as `xwayland: true`, floating and pinned; it did not take
     focus (`hyprctl activewindow`); a drag moves it, and the place survives a
     restart (X2).
  4. Secrets: save a password in Chromium, set an API key, then reboot (the
     pairing from step 0 stands). Pass when no keyring
     prompt appears at boot; `journalctl -b | grep -i 'unrecognized format'` is
     empty; `secret-tool search --all service uno.schmid.steno.mac` lists
     `llm-api-key` and `handover-identity`; `~/.local/share/Steno/secrets.json`
     does not exist; Chromium's saved passwords still open (P8).
  5. The phone uploads a recording through the firewall (X4).
  6. Steps 4 to 6 of the GNOME gate, with the call in Chromium and the logout
     through the system menu, which is `uwsm stop`. Also `hyprctl dispatch exit`
     while recording: after logging back in, the meeting is saved with `quit` as
     `endReason` (X1).
  7. In the Steno that a login started, Settings says updates come from the
     package manager (X5), and `~/.config/autostart/steno-desktop.desktop`'s
     `Exec` names `/usr/bin/steno-desktop`.
- **NixOS** (x86_64-linux, GNOME, and Hyprland with `withUWSM = true` if
  Nicolai runs it).
  1. Step 0 uses a flake input at the previous candidate's tag
     (`github:NicolaiSchmid/steno/v0.11.0-rc.<N-1>`), with
     `steno.nixosModules.default` in the system's modules and
     `programs.steno.enable = true`.
  2. Steps 2 to 4 and 6 of the GNOME gate, with the phone through the firewall
     the module opened.
  3. Open Settings and choose a folder: the file chooser opens (the wrapper's
     schemas).
  4. Launch at login: after step 0's upgrade and collection, `systemctl --user
     cat steno.service` names the profile path, and About shows the new version.
  5. Settings says updates come from the package manager.
  6. A package-only install: set `programs.steno.enable = false`,
     `nixos-rebuild switch`, and log out and in so the module's Steno is gone;
     `nix profile install
     github:NicolaiSchmid/steno/v0.11.0-rc.<N-1>#steno`, launch at login on;
     replace it with the candidate under test (`nix profile remove steno`, then
     `nix profile install github:NicolaiSchmid/steno/v0.11.0-rc.N#steno`);
     `nix-collect-garbage -d` as that user; log out and in: Steno starts once,
     as the new version, and its file chooser opens.

**The Windows gate** (before the site lists Windows): a Windows machine runs the
`--ignored` WASAPI tests, one real call, a logoff during a recording of at least
an hour that saves it (P5), and a `kill` that P3 recovers.

## Order of operations and gates

1. **Every decision is confirmed** (2026-10-07). The call-mode row of D9
   closes from A9's check in step 2.
2. **Every package is written,** in parallel except for the dependencies under
   "Work packages": S1 to S7, A1 to A9, P1 to P38, X1 to X7. Any further gap the
   data-loss audit finds joins the P table: before G1 it lands with the others;
   after G1 it lands as a new candidate (G2).
3. **Gate G1.** Every S, A and P row and X1 to X7 are closed, the pull requests
   under "Also landing before the first candidate" are merged, and the audio
   path is final (D9). X8 runs in G2.
4. **Bump to `0.11.0-rc.1` and tag it.** This publishes a pre-release and moves
   `desktop-beta`; no handoff item is signed into the appcast. Desktop-id
   installs convert.
5. **Gate G2.**
   - R1 to R3 pass on candidate N. R4 publishes N+1, which changes only the
     version, and R5, R6 and the dogfood pass on N+1, in that order.
   - The Linux gate of each target to be listed passes.
   - The R5 soak and the stability count start only on a candidate built after
     the last P package, including any the audit adds after G1 (D3).
   - **The stability count** (D12): R5's soak passes, and ten real meetings on
     the Mac and five on each Linux target to be listed are recorded, processed
     and exported without a failure. The deliberate kills, logouts and reboots
     of the gates do not count. Each count includes a meeting over an hour, a
     phone upload and a device switch during a call. A failure is:
     - a recording more than 10 s shorter than the time from start to stop;
     - a lane with no transcript segment while it carried speech;
     - a job that fails and is not fixed by Process again (P9);
     - a crash: a `crash-<UTC>.log` in the support directory dated within the
       count's window (P38), or any
       `steno-desktop` or `steno-speech-sidecar` report in
       `~/Library/Logs/DiagnosticReports` or in `coredumpctl list`.

     Nicolai keeps the tally (meeting, platform, candidate, result) in one
     GitHub issue.
   - A fix becomes a new candidate. The steps it touches run again, and a fix to
     the audio path or a new P package restarts the R5 soak and the stability
     count.
6. **Before the stable tag, Nicolai:**
   - creates the `appcast` environment, with himself as required reviewer,
     "Prevent self-review" off and deployment tags `v*`; `gh api
     repos/NicolaiSchmid/steno/environments/appcast --jq
     '[.protection_rules[].type]'` includes `required_reviewers` and
     `branch_policy`;
   - has his AUR account ready for the first push (X6).
7. **Bump to `0.11.0` on a fresh commit and tag it.** This publishes the full
   release as "latest", creates `desktop-stable`, moves `desktop-beta` to
   0.11.0, uploads `appcast.xml`, and puts the flake lines in the summary. The
   `handoff` job waits for approval. Nicolai opens the flake bump PR.
8. **Gate G3.**
   - R7 passes.
   - Then Nicolai approves the `handoff` job, makes the first cask bump by hand
     (D7) and the first AUR push (X6), and R8 passes.
   - Merge S8 and the flake bump; Vercel deploys the site. Nicolai sets
     `HOMEBREW_TAP_TOKEN` and `AUR_SSH_PRIVATE_KEY`.
9. **Rollback window: 14 days.** No database migration merges. Watch the
   issues.
10. **S9**, the Swift removal.

## Rollback

**Before step 7**, nothing reaches Swift users or stable installs. A bad
candidate is handled as the desktop README's "A bad release" says for
`desktop-beta`.

**If R7 fails,** Nicolai rejects the `handoff` job and runs `gh release edit
v0.11.0 --prerelease`. No Swift build has seen the item and the cask is not
bumped. Candidate installs take 0.11.0 through `desktop-beta`, and a fresh
download in that window reads `desktop-stable`; if the failure is in the
bundle, put both lanes back as "A bad release" says. The fix ships as `0.11.1`.

**A bad handoff item, while it rolls out:**

1. Revert the item's commit on the `appcast` branch, then `gh release upload
   v0.11.0 appcast.xml --clobber` with the reverted file, and allow five minutes
   for raw.githubusercontent's cache.
   - Installs that have not downloaded the item stop seeing it.
   - Installs that downloaded it with automatic download on install it when they
     quit.
   - The phased rollout keeps that group small.
2. If fresh installs are affected too, `gh release edit v0.11.0 --prerelease`
   takes the release out of "latest" (`releases/latest` then answers 404, which
   also cuts off build 244's feed). In the same hour, take the platform off the
   site's released list.
3. Revert the flake bump; Nicolai reverts the tap commit and the AUR bump.

**Users who already took a bad release** cannot go back through Sparkle. Fix
forward with `0.11.1`, which the daily check (S4, R4) delivers. Stop the spread
on the lanes as "A bad release" says; the first stable release has no earlier
release on `desktop-stable`, so there `latest.json` is deleted (`gh release
delete-asset desktop-stable latest.json`); S8 adds this to that README section.

**As a last resort for one user:** quit Steno, then drag the `v0.10.0-rc.2` app
from its DMG onto the Tauri app in Finder and choose Replace. It still has its
own TCC grants and reads the shared API key, and it opens the v5 database (R6).
Then either revert the item for everyone before relaunching it, or, for this
user alone, run `defaults write uno.schmid.steno.mac SUAutomaticallyUpdate -bool
false` while it is still quit, open it, and choose Skip This Version when 0.11.0
is offered.

**An install of `desktop-v0.1.0-rc.*`** that meets a v5 database exits at launch
and cannot update itself; the release notes say to install the current build by
hand. The database is untouched (R6).

**The database.** Migrations only add (Tags and versions), v5 lands before the
first candidate, every migration until S9 is mirrored in `Migrations.swift`, and
none merges in the window. The last Swift release and every Rust build from P2
on open a newer database, so going back stays possible until the Swift app is
removed.

## Changes to other plans in this plan's PR

- **`.plans/2026-10-04-mac-cutover.md`.** A note at the top says that this plan
  extends it, where it changes it, and which details no longer hold.
- **`.plans/2026-10-02-rust-core-and-tauri-shell.md`.**
  - The status line, the WP9b progress row and the owner sentence of "Open after
    the port" point here, and that sentence says that an item that can lose data
    blocks the stable release whatever its owner, and that an item a row of this
    plan's tables names belongs to that row's package.
  - These places now point at this plan: the Transition paragraph and WP9's
    opening line (the new identifier); WP9's cutover paragraph; the "Updates"
    and "Pending speaker reviews" lines under "Beyond the bridge"; seam (4); the
    first **WP9b.** item; the Shell section's login-item line (D4); the
    **WP9b.** item on the other Swift fixes (D9; S7 deletes it, apart from call
    mode, which A9 deletes).
  - The closed **WP9b.** item about the first `desktop-v*` tag is deleted. The
    other items stay until the pull requests that fix them delete them; the
    packages named here take them over.
