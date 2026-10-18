# Stable promotion: the Tauri app becomes the Steno release

Status (2026-10-18): every work package is merged except S8, the site and
README (#303, open until R8 passes), and S9, the Swift removal after the
rollback window. Gate G1's packages are all in. No `0.11.0-rc.1` is tagged
yet. Left: the speaker-clip follow-up (Open work), the rehearsal R1 to R8 and
the dogfood, the Linux and Windows gates, the stability count and Nicolai's
checks (all under "Order of operations and gates"), then the stable tag, the
handoff and the 14-day rollback window. `.plans/2026-10-18-simplify.md`
(steps 4 and 5) removes several guards this plan built; the rows and gate
steps it gives up say so.

Nicolai decided on 2026-10-07 to promote the Tauri app from beta to stable:
release tags and the build pipeline produce the Tauri app, the landing page
offers it, and existing users of the Swift Mac app end up on it without losing
meetings, settings, secrets, paired phones or the login item. His rule for the
whole promotion: no data is ever lost (D3). He confirmed every decision on
2026-10-07; the decision table shows what changed.

This plan extends `.plans/2026-10-04-mac-cutover.md`: the cutover's steps 1
to 6 became S1 to S7 here, with a new bundle id (D5), and its step 7 is S9. The
rehearsal replaces the cutover plan's tests.

In `.plans/2026-10-02-rust-core-and-tauri-shell.md`, this plan owns every
**WP9b.** item in "Open after the port", and every item there that can lose
data (D3).

## Facts this plan rests on

Checked on 2026-10-07 against `main` at `2f9a253b` (#217's merge), the
releases, the `appcast` branch, the released bundles, the GitHub API, the
Sparkle 2.10.0 source and tarball, GRDB 7.11.1, the Tauri 2.10 bundler, Apple's
Security sources, Omarchy 4.0.4 and nixpkgs. The rollback facts were run on
Forge and atlas.

- **Every release so far is a pre-release.** Swift: `v0.9.0-rc.1` to
  `v0.10.0-rc.2`. Tauri: `desktop-v0.1.0-rc.1` to `desktop-v0.1.0-rc.3` and the
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
  (`v0.10.0-rc.2`). The Tauri build number is the commit count of `main`, far
  above 542 (Tags and versions).
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
    password. "Always Allow" puts the new app on the item's list for good;
    "Allow" lets this one read through and asks again next time;
  - the handover identity's private key was imported with the default access,
    which trusts only the Swift app, so exporting it asks too ("wants to export
    key", with the password);
  - the `SMAppService.mainApp` registration belongs to the Swift bundle id, and
    an app can only register or unregister itself. What the stale entry does
    after the swap is not documented;
  - the Swift app's preferences stay in the `uno.schmid.steno.mac` domain.
- **The keyring service is not the identifier.** Every Steno keyring entry is
  filed under the service string `uno.schmid.steno.mac` (`KEYRING_SERVICE`) on
  every platform. It stays, so no secret is filed under a new name.
- **`com.nicolaischmid.steno` is the iOS app's production bundle id**
  (`mobile/app.config.ts`). App Store Connect offers an iOS app on Apple
  silicon Macs, so a desktop app with the same id would collide with it (TCC
  rows, Launch Services). The desktop app therefore takes
  `com.nicolaischmid.steno.desktop` (D5).
- **The phone finds the computer by `macID`, not by name.** `macID` is derived
  from the handover certificate's fingerprint (`HandoverIdentity::mac_id`), and
  the phone looks the service up by it. A new identity therefore breaks every
  pairing. A desktop-id build (`uno.schmid.steno.desktop`, the beta's
  identifier) has minted its own identity.
- **Omarchy 4** (Arch with Hyprland, Wayland): its bar's tray is an SNI host
  behind a hover drawer, where a left click on Steno's icon may do nothing and
  a right click opens the menu; the panels need Lua window rules
  (`o.window(...)` in `~/.config/hypr/`); `ufw` denies every incoming port but
  LocalSend's 53317; gnome-keyring runs a plaintext, always-unlocked default
  keyring, which a multi-line secret corrupts (omacom/omarchy #5105).
- **Asset names differ.** The Swift release carries `Steno-<v>.dmg`. The Tauri
  release carries `Steno_<v>_aarch64.dmg` and `.app.tar.gz`,
  `Steno_<v>_x64_en-US.msi` and `_x64-setup.exe`, and
  `steno-desktop_<v>_amd64.deb` and `.AppImage`.
- **Rollback over a v5 database** (run on Forge and atlas):
  - P2 added a fifth migration in both apps, a new table with no foreign key
    and no change to existing tables;
  - Swift `v0.10.0-rc.2` (GRDB 7.11.1) opens a v5 database normally: the
    migrator ignores an applied migration it does not know, never erases while
    `eraseDatabaseOnSchemaChange` is false, and the app never calls
    `hasBeenSuperseded`. It then keeps writing, and nothing is lost;
  - the Rust `desktop-v0.1.0-rc.*` builds refuse it (`UnknownMigration`) and
    panic at launch with no dialog. The file is untouched, but the in-app
    updater cannot run, so such an install needs a newer build installed by
    hand. Every build from P2 on ignores later migrations with a warning, as
    GRDB does.

## Decisions

| | Before 2026-10-07 | Nicolai's answer | Now |
|---|---|---|---|
| D1 | `v*` tags | as recommended | Confirmed |
| D2 | 0.11.0 | as recommended | Confirmed |
| D3 | the blocking list gates data loss, core flows and silent failures | "we should never loose any data" | Changed: every data-loss path blocks (P1 to P39; P40 is a stability item) |
| D4 | `SMAppService`, the Swift registration carrying over | as recommended | Confirmed; with D5 the app registers itself (S6) |
| D5 | `uno.schmid.steno.mac` on the Mac, `uno.schmid.steno.desktop` elsewhere | "it should be com.nicolaischmid.steno", then "Desktop gets .desktop" | Changed: `com.nicolaischmid.steno.desktop`; directories decoupled; confirmed |
| D6 | Linux and Windows listed once gated | "also omarchy and nixos users" | Changed: GNOME, Omarchy and NixOS, X1 to X8 |
| D7 | the cask follows stable releases only | as recommended | Confirmed |
| D8 | one frozen handoff item | as recommended | Confirmed |
| D9 | the parity differences accepted | "audio processing stable and ideally the final version" | Changed: the audio path final before the first candidate; the final choices confirmed |
| D10 | `steno-macos` | as recommended | Confirmed |
| D11 | none | the lost-complete fix before stable | New, decided by Nicolai |
| D12 | the dogfood: five meetings in three days | confirmed as proposed | New, confirmed |
| D13 | none | "Write beside" | New, confirmed |

- **D1 Tags are `v*` for the Tauri app.** One product, one tag line.
  `release.yml` is gone (S7). The updater lanes keep the names
  `desktop-stable` and `desktop-beta` for good, because installed desktop
  builds have them compiled in. The old `desktop-v*` tags stay.
- **D2 The version line continues at 0.11.0.** Candidates are `0.11.0-rc.N`;
  `1.0.0` remains the name for the day the v1 checklist (issue #74, re-scoped
  to the Rust app) is done.
- **D3 Nothing that can lose data ships.** Nicolai: "we should never loose any
  data. if there are gaps, then we need to dispatch more agents." Every known
  path that can lose a recording, a transcript, a note or a pairing blocks the
  stable release, whatever its likelihood, on every platform the release
  publishes (Windows included though unlisted). A Swift-only path closes with
  the handoff (D9), since Swift ships no further release and runs again only as
  a rollback. Everything that loses no data follows (What follows). The
  simplify plan (2026-10-18) narrows this: it gives up the Linux logout and OOM
  kills and the NAS flush case it names under "Given up", and it removes the
  crash-loop quarantine (A17's sub-rule that a crash loop counts as data loss);
  a reported loss gets a small fix aimed at that case.
- **D4 The login item stays `SMAppService.mainApp` on macOS,** through the safe
  `smappservice-rs`, with the General section's `requiresApproval` copy. With
  D5 the new app registers itself (S6). Linux and Windows keep
  `tauri-plugin-autostart`, with X5's rules.
- **D5 The app's identifier is `com.nicolaischmid.steno.desktop`,** on every
  platform, replacing both old ones (`tauri.conf.json`). The iOS app keeps
  `com.nicolaischmid.steno`; mobile's native identifiers are not touched.
  - **What it costs:** every Swift user grants microphone, system audio and
    calendar once more; macOS asks once for the API key and once for the
    handover key, each with the login keychain password (choose Always
    Allow); the Swift Login Items entry is left to macOS (S6, R3); on a Mac
    that also holds a Swift identity, phones paired only with a desktop-id
    build pair again, since the Swift identity wins.
  - **Directories: decouple, do not move** ("Stop depending on the ID"). The
    support directory was never named after the identifier. The panel anchor
    lives in it (`Steno/panel-anchor.json`), read once from the desktop-id
    build's config directory when missing, so nothing that matters is named
    after the identifier and a later id change costs nothing.
- **D6 Linux targets GNOME, Omarchy and NixOS; Windows ships unlisted.** Every
  tag publishes all three platforms. The site lists a Linux target once its
  gate passes on Nicolai's machine: GNOME (the `.deb` or the AppImage),
  Omarchy (the AUR package `steno-desktop-bin`) and NixOS (the flake's package
  and module). Windows is listed once a Windows machine has run its gate. A
  gate blocks its target's listing, not the stable tag.
- **D7 The cask follows stable releases only.** The first stable bump is
  **Nicolai**'s tap commit; after it he sets `HOMEBREW_TAP_TOKEN`, and the
  workflow bumps from the second stable release on.
- **D8 One frozen handoff item.** The `appcast` branch gets one item without a
  channel, for `0.11.0`, with Sparkle's phased rollout
  (`--phased-rollout-interval 86400`: seven groups, one day apart, counted from
  the approval; Check for Updates skips the wait). The same feed is the
  `appcast.xml` asset of every stable release, so `v0.9.0-rc.1` finds it
  through `releases/latest`. Only a later release that fixes the handoff itself
  publishes a new item, which supersedes this one.
- **D9 The audio path is final before the first candidate, and the remaining
  Swift fixes close at the handoff.** Nicolai: "we need to make sure the audio
  processing is stable and ideally the final version before we cutover to the
  tauri version."
  - No change to the audio and speech path is planned after the cutover;
    every A package is merged, so the rehearsals run on the final pipeline. A
    fix found during the candidates becomes a new candidate and restarts the
    stability count (G2). A later change to the capture's loss counting needs
    a new D9 ruling (What follows).
  - **The final choices** (confirmed 2026-10-07 and 2026-10-08):

    | Difference | Choice |
    |---|---|
    | Mixdown format | 16 kHz mono WAV on every platform: one code path, no encoder dependency; about 115 MB per hour in the vault instead of about 30 |
    | Resampler (44.1 kHz phone audio) | Final, the 7 to 8 kHz fold included (Nicolai accepted it on A9's sweep, 2026-10-08): aliases below 7 kHz stay under -60 dB; FLEURS German 5.02 % WER through the 44.1 kHz path, 5.51 % through 48 kHz |
    | Sidecar's 2 ms lag | Accepted, pinned by `tests/codec.rs` |
    | AAC priming | Trimmed from the edit list, else iTunes' gapless tag, else the 2 112 samples AVFoundation assumes, only in the phone recorder's layout (A9) |
    | Call mode without an output client | Fixed (A10): the tap includes Steno, so no in-app playback while recording, enforced by `steno_audio::Playback` |
    | A phone recording without a `moov` | Salvaged into a meeting that says it was cut off (A14) |
    | A capture thread that misses cycles | Written as silence or counted, and the note after the stop says how much is missing (A18) |
    | A damaged AAC packet | Silence of its duration, shown on the meeting; more than half failed is an error (A12) |

  - **A known limit, A11** (Nicolai, 2026-10-12): a Bluetooth headset that is
    the system output may switch to hands-free while a call is recorded,
    because recording the call also opens the headset's microphone. It costs
    quality only, no audio is lost, and the Swift app is unchanged. It is
    revisited after stable; A10's step 6 notes whether it happens on
    Nicolai's Mac.
  - The Swift defects and the parity notes' other "before cutover" ports to
    Swift close with the handoff. No Swift bridge release ships (Nicolai,
    2026-10-15: "if it migrates seamlessly in mac, or at least i can make codex
    fix it, we don't need a bridge. currently i'm the only user."): the Swift
    app ships no further build and runs again only as a rollback
    (`v0.10.0-rc.2`). The move from the Swift app to the Rust app on Nicolai's
    Mac must be seamless: anything that could break it is fixed before the
    handoff, or, where only his Mac can show it, is a step on his gate list
    (Rehearsal, "Nicolai's Mac"). The fixtures the Swift side owes are dropped
    at S9, when the Rust fixtures become the contract.
- **D10 `steno-macos` holds the Mac framework calls without a safe binding**
  (the identity export's `SecItemExport`, EventKit, the keychain test's
  fixture calls), each in a safe wrapper with a comment on every invariant,
  and `AGENTS.md`'s `unsafe` list names it and every module that holds
  `unsafe`. Done with S6 and S3.
- **D11 The handover's lost-complete-answer fix lands before the stable
  release.** Decided by Nicolai on 2026-10-07. It is P2, with the fifth
  migration in both apps; the 14-day no-migration rule applies after the
  stable tag.
- **D12 The stability count is ten real meetings on the Mac and five on each
  Linux target to be listed.** Ten meetings cover a working week's calls on
  the Mac, the platform with every Swift user; five per Linux target cover its
  capture and session paths. The rules are in G2.
- **D13 A re-export never overwrites a note the user edited in the vault.**
  Nicolai: "Write beside." If a note changed since Steno wrote it, Steno leaves
  it alone, writes the new version beside it as `<name> (Steno <date>).md`, and
  shows a warning (P30). Carrying the edited note's `[x]` task ticks over into
  the new version is not part of it.

## Closed packages

Every package below is merged on `main`. The letters: S for the Mac and the
release, A for the audio path, P for the other data-loss fixes, X for the
Linux targets. Each line keeps what later work and the gates rely on; the
code and the pull request hold the rest. "Simplify" means
`.plans/2026-10-18-simplify.md`.

**S: the Mac and the release**

- S1: done (#237). The Mac downloads CoreML Parakeet v3
  (`FluidInference/parakeet-tdt-0.6b-v3-coreml`, pinned) into
  `fluidaudio/parakeet-tdt-0.6b-v3`; a stored Whisper, Ultra or DE engine
  becomes `parakeet-v3` with a one-time notice; the pipeline never downloads:
  a meeting that lacks a model waits queued with "Download the speech models
  in Settings" and resumes once they are installed; WeSpeaker's licence is
  CC BY 4.0, shown with attribution.
- S2: done (#271). Meeting detection's prompt and the auto-stop 90 s after a
  call, which stops through the Stop path with `callEnded`; while the capture
  recovers from a device change the countdown waits, for one grace at most.
- S3: done (#288). The calendar event at recording start, and the
  permissions: `unknown` counts as missing on the Mac only, and a missing
  grant opens onboarding on its permissions page whatever
  `steno.onboardingCompleted` says.
- S4: done (#258). A daily update check, due at launch and hourly when the
  last check time (`lastCheckAt` in `update-check.json` in the support
  directory, RFC 3339 UTC) is missing or more than 24 hours old; the flags
  `steno.updates.automaticChecks` (missing: on) and
  `steno.updates.automaticDownload` (missing: off) in `preferences.json`; the
  pairing QR code.
- S5: done (#247). The Bonjour record follows a network change, and the Mac
  advertises its computer name.
- S6: done (#268, #236, #281, #289, #294). The identifier (D5); `Info.plist`
  carries the Swift `SUPublicEDKey`
  (`RxaX7phoHvb7M0P4yaOC7zngDo+lqlOE6Iq89UtOuQI=`); `steno-macos` (D10).
  - **The import** (macOS only, `steno_services::swift_import`) runs once: it
    copies `steno.onboardingCompleted` and Sparkle's `SUEnableAutomaticChecks`
    and `SUAutomaticallyUpdate` into S4's flags, drops the panel anchor,
    removes a desktop-id build's Launch Agent, then, on an onboarding step,
    reads the API key (one prompt) and exports the handover identity (one
    prompt) into the PEM entry `handover-identity`. It records
    `steno.swiftImportRan` in `preferences.json` and the keychain entry
    `swift-import-done`; either one stops it from touching the identity again.
  - **Never mint over a pairing:** a denied or failed export, or an identity
    that cannot be read, leaves the listener off with Try again in Settings >
    iPhone; only the user's Pair again mints a new identity, and it writes the
    `swift-import-done` marker first. The rule is one pure decision,
    `steno_handover::identity::decision::decide`, and its truth table,
    `crates/steno-handover/tests/fixtures/identity-decisions.json`, is the
    statement of it (Nicolai's decision of 2026-10-15).
  - **The login item** (D4): the new app registers itself when the stored
    `launch_at_login` is on. The Swift entry is left to macOS. If R3 shows that
    it stays as a second "Steno", launches a Swift copy or blocks the new app's
    registration, the onboarding step and the release notes tell the user to
    remove it in System Settings > General > Login Items; R3 checks that the
    words lead there.
- S7: done (#274). Everything under "Release mechanics", `release.yml`'s
  deletion included.

**A: the final audio path (D9)**

- A1: done (#228). A streamed decoder and mixdown; R5's soak bound is 6 GiB
  for the app and its sidecar together.
- A2: done (#263). CoreML on the shared chunker; the accuracy gate is FLEURS
  German `cat/` through CoreML at or below 5.0 % mean WER
  (`scripts/coreml-parity-forge.sh` prints PASS or FAIL), with the Swift
  corpus as a drift check under 10 %; measured 4.87 %.
- A3: done (#229, #266). The diarizer's models come through `ModelStore`, and
  its inference runs in the speech sidecar on every platform (invariant 4).
- A4: done (#214). PipeWire's `stop()` is bounded; `start`'s first cycle and
  latencies on Nicolai's GNOME and Omarchy machines are measured in the Linux
  gates.
- A5: done (#218). A6: done (#198). A7 and A8: done (#222), the Linux input
  device list and meeting detection over PipeWire.
- A9: done (#246). The final choices in D9's table, proven.
- A10: done (#251). Call mode records from its start with nothing playing;
  Nicolai's steps on his Mac are under "Order of operations and gates".
- A11: a known limit (D9), nothing before stable.
- A12: done (#264). A13: done (#279), word starts from a chunk's exact start.
- A14: done (#280). A phone recording without a `moov` is salvaged; the
  phone-side check is Nicolai's (G2).
- A15: done (#296). CoreML's IOSurface growth and Objective-C exceptions are
  contained; the debug-build check is Nicolai's (G2).
- A16: done. A16a (#310), the owned-span partition; A16b (#317), the ONNX
  engine's lead-in and tail (FLEURS 4.865 %).
- A17: done (#298). Allocations are bounded before symphonia opens a file;
  the quarantine of a recording whose read ended the app twice is removed by
  simplify step 4.
- A18: done (#297). Missed capture cycles are written as silence or counted
  on PipeWire, Core Audio and WASAPI, and the note after the stop says how
  much is missing; WASAPI's fill is unit-tested only, so the Windows gate's
  stalled recording is its hardware check. PipeWire's data-loop thread without
  real-time scheduling logs a warning, which the Linux gates check.
- A19: done (#311). No CoreML load or run beside a live recording. Its
  warm-up mark and the switch to the sidecar after a CoreML abort are removed
  by simplify step 4.
- A20: done (#308). The level meter runs on a thread of its own.

**P: no data lost (D3)**

- P1: done (#213). The phone intake commits its receipt, meeting and asset in
  one durable transaction before `complete` answers.
- P2: done (#243). The v5 `handoverAdmission` table answers a re-sent
  `complete` with the meeting it made; the Rust migrator ignores later
  migrations with a warning and the desktop shows a dialog instead of
  panicking (D11).
- P3: done (#233, #248). A recording ended by a kill is salvaged at the next
  launch with the existing end reason `failed` (no new enum value, so the
  Swift app still reads the row), and a master with no row is adopted.
- P4: done (#244). P5: done (#227); the systemd stop-timeout drop-ins are
  removed by simplify step 4. P6: done (#260); the sidecar's own systemd scope
  is removed by simplify step 4.
- P7: done. A people folder typed as `./People` becomes `People`, and `.`
  no people folder, as Swift wrote it.
- P8: done (#221). Every secret written to the Secret Service is one line
  (the PEM bundle base64-encoded), since a multi-line secret corrupts
  Omarchy's keyring; this holds for the `keyring` client of simplify step 5
  too.
- P9: done (#240). Process again runs a failed meeting with its master from
  the start, from the button and from `steno process --meeting <id>`; the
  Swift app refuses it.
- P10, P11, P12: done (#231). P13: done (#231, #228); its resume-attempt
  guard is removed by simplify step 4.
- P14: done (#241). Every retention stamp commits durably, and an incomplete
  meeting's recording stays unstamped; its `damaged-audio.json` input is
  removed by simplify step 4.
- P15: done (#225). One exclusive lock beside the database; on the Mac the
  Rust app refuses to start while the Swift Steno runs in the same session.
- P16: done (#235). P17, P18, P20, P38: done (#230; the Mac's disk floor
  #285). P19: done (#222, #244). P21, P23: done (#226). P22: done (#244). P24:
  done (#228).
- P25: done (#270); simplify step 5 replaces the install gate, the hold and
  the Windows watcher with the Tauri updater's default behaviour.
- P26, P27, P29: done (#234). P28: done (#235); the export-retry count is
  removed by simplify step 4. P30: done (#238), D13.
- P31, P32: done (#232). A settings save upserts and keeps keys it does not
  know.
- P33: done (#219, #212). P34: done (#223).
- P35: done (#282, #294). The live Keychain check on a spare account is still
  Nicolai's (G2).
- P36: done (#242, #262); simplify steps 4 and 5 replace the hand-written
  Windows durable writes and the Windows credential client.
- P37: done (#220); simplify step 4 removes the Linux session-end handling.
- P39: done (#278). A run that meets the store locked by another process
  leaves the meeting `queued` and retries, instead of failing it.
- P40: done (#286, #299, #304). Store calls in async code go through one
  facade over `spawn_blocking`, reads use reader connections, `recording`
  comes from a feed of its own, and reloads run only when the store changed;
  the host's writes keep the view-model lock until
  `.plans/2026-10-15-host-actor.md`. Its follow-up, the copy a phone admission
  left is one meeting, is done (#307).
**X: the Linux targets (D6)**

- X1: done (#220, #272); the own systemd scopes and the AppImage mount
  server's scope are removed by simplify step 4.
- X2: done (#267). The panels stay on XWayland under Hyprland, titled "Steno
  bubble" and "Steno prompt", with the Lua rules in
  `apps/desktop/src-tauri/linux/hyprland-steno.lua`.
- X3: done (#275). Every tray action is in its menu, and with no tray host,
  closing the main window quits and saves.
- X4: done (#276). TCP 23820 on Linux (`HandoverConfiguration::LINUX_PORT`),
  the AUR package's `ufw` profile `Steno`, and the NixOS module's
  `programs.steno.openFirewall`.
- X5: done (#261, #305). Packaged installs (`STENO_DISTRIBUTION`, a binary
  under the Nix store) leave updates to the package manager, and the autostart
  entry names a stable path; `STENO_LOGIN_ITEM=managed` leaves launch at login
  to the system.
- X6: done (#265). The AUR package `steno-desktop-bin` (`packaging/aur/`). The
  first stable push is **Nicolai**'s; then he adds `AUR_SSH_PRIVATE_KEY`, and
  `publish` pushes from the second stable release on. Publishing the release
  key to keyserver.ubuntu.com is his too.
- X7: done (#259). The flake's Linux package and the NixOS module
  (`programs.steno.enable`, `steno.service`); FLEURS 4.9 % with nixpkgs' ONNX
  Runtime.
- X8: the Linux gates (Rehearsal), open.

## Open work

- **S8 Site and README** (#303, `docs/stable-release-pages`). Written during
  the candidates, merged after R8 passes; the content is under "Landing page
  and README".
- **S9 Swift removal.** The cutover plan's step 7, after the 14-day rollback
  window, as step 7 of the simplify plan: the web app moves to `apps/web`, the Swift
  rows leave `AGENTS.md`, `apps/macos/scripts` goes (what the desktop workflow
  still calls moves to `apps/desktop/scripts`), and issue #74 is re-scoped or
  closed.
- **Leftover speaker clips can outlive the retention stamp** (a privacy item,
  as the clips are voice snippets; found in #313's review). The retention
  sweep removes the clip files no speaker row names through `sweep` in
  `crates/steno-pipeline/src/sample_clips.rs`, and that pass never sets
  `clean = false`: a `read_dir` error reads as an empty folder, and a clip
  that cannot be removed is only logged. So the master goes, the stamp clears,
  and those clips stay until the meeting is deleted. It needs clips a run left
  behind, and then a speakers folder that alone cannot be searched or a clip
  that resists removal. Planned fix: `sweep` reports its failures, a
  `read_dir` error other than `NotFound` counts as one, and
  `RetentionSweep::run` ANDs the result into `clean`, so the stamp stays for
  the next sweep. No schema change.

## What follows

These lose no data and block nothing.

- Layer-shell panels on wlroots, Hyprland and Plasma (X2's rules cover them
  until then).
- The phone records in a crash-safe container (CAF, or fragmented MP4), so a
  recorder killed mid-recording leaves a file any reader opens, not one only
  A14's salvage reads. It changes the recorder's settings and moves the native
  fingerprint, so it needs a TestFlight build (`mobile/README.md`).
- Pair this phone again, per phone. In the mixed case (some phones paired
  before the identity changed, the others after it) Settings could keep the
  newest identity and the phones that know it, and offer Pair this phone again
  on the others alone. Until then Pair again pairs every phone again.
- The handover identity from evidence, not times (Nicolai's decision of
  2026-10-15). After the handoff, its 14-day rollback window and S9, as no
  migration lands in the window:
  - a v6 migration adds a nullable `pairedDevice.identityFingerprint`, written
    in the pairing's own durable transaction. Rows from before stay NULL, and
    a launch that serves the recorded identity fills them in, so the
    pairing-time rule survives only for the one-time Swift import. Rust only,
    with no `Migrations.swift` mirror after S9;
  - the import's done flag moves into `handover-identity.json`, written in the
    same replace that records the imported fingerprint. The keychain marker
    (`swift-import-done`) and `steno.swiftImportRan` are still read for good,
    and still written while a Rust build that reads only them could be
    installed again after a rollback.
- The speech settings without a Settings row, which
  `.plans/2026-10-07-speech-settings-ui.md` plans; the untested `steno-llm`
  cleanup paths.
- zbus 6, once `tauri-plugin-opener` and `tauri-plugin-single-instance` move
  to it, and the workspace's own `zbus` with them; until then `deny.toml`
  ignores RUSTSEC-2026-0370 to 0372.
- One loss ledger per stream (after A18): each stream compares the frames its
  clock expected with the frames its thread delivered, reconciled once at its
  stop, and the session merges the lost spans by union, so nothing is
  subtracted. It is the first capture PR after stable, with the recorder actor
  plan's capture seam and fake, and it needs a new D9 ruling.
- A11, the headset's drop to hands-free during a recorded call, revisited.

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

### The release workflow

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
    - push the AUR bump once `AUR_SSH_PRIVATE_KEY` exists, and open a pull
      request with the same bump to `packaging/aur/` (X6);
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
      phones waiting and offers Try again in Settings > iPhone, and phones pair
      again only where D5 says so or the user chooses Pair again;
    - the optional mixdown is now WAV;
    - Whisper, Ultra and DE now transcribe with Parakeet v3;
    - Homebrew users whose app updated itself can run
      `brew upgrade --greedy --cask nicolaischmid/tap/steno`;
    - the install lines for Omarchy (`yay -S steno-desktop-bin`) and NixOS;
    - the Linux known issues: the KDE Plasma logout until Plasma calls the
      portal monitor, the whole default sink as the system lane, the WebKitGTK
      descriptor leak on very long sessions, no tray on GNOME without the
      AppIndicator extension (closing the main window then quits Steno and
      saves a recording), and a Steno started outside `uwsm` on Hyprland.
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

No Swift bridge release ships (D9): `release.yml` is not restored. If the
rehearsal shows that a Swift build cannot take the handoff, the fix is on the
Rust side or in the handoff item, and it blocks the handoff; the Swift Login
Items entry is the user's to remove, as the onboarding step and the release
notes say (S6).

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
`Contents/MacOS/steno-desktop` and
`bundleId = "com.nicolaischmid.steno.desktop"`, and its UPDATES text says
a Nix-installed Steno offers no update and Settings, under General, says
"Updates come from your package manager." (#305); X7 adds the Linux
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
| Speaker models | The diarizer's ONNX models (about 33 MB) are new: onboarding opens on its permissions page while they are missing, even with every grant in place, and its Speech models row downloads them; the meetings waiting for them then resume | R3; "Nicolai's Mac", step 17 |
| Obsidian vault | A vault under Documents, Desktop, Downloads or iCloud Drive, or on an external disk or a network share, needs folder access for the new identifier: the permissions page writes a hidden empty file to it once and removes it at once, so macOS asks there, and the row says what to do after a Don't Allow; a leftover from a crash is removed at the next check, and a folder that refuses deletes reads as not writable once its second probe stays, so it holds two at most | R3; "Nicolai's Mac", step 18 |
| Local Network | macOS asks once, when the new app starts listening for the phone; Settings > iPhone says where to switch it on if the phone cannot find the Mac | R3; "Nicolai's Mac", step 16 |
| A setting the new app cannot read | It refuses to start; the dialog names the setting's key and the database, the log names the key and the problem only; neither holds its value | "Nicolai's Mac", step 6 |

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
  - the app relaunches as `com.nicolaischmid.steno.desktop` (one a login started may
    quit instead; open it once), and launch at login starts it once;
    `~/Library/LaunchAgents` holds no Steno plist, and with launch at login off
    a login starts nothing;
  - on the Mac, an earlier build's agent switched off under "Allow in the
    Background" while the setting stayed on opens Steno at login again after
    the update, as expected: the agent's file goes, the first launch registers
    the main app, and turning the setting off in General removes it;
  - on the Mac, the main app removed under Login Items after that first launch
    is not added back at the next launch, and General shows the switch off;
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
  - both calls are listed, transcribed and summarised;
  - on the Mac, besides the prompts above, two more are expected: the Local
    Network prompt, once, when the new app starts listening for the phone
    (Allow), and, when the vault is under Documents, Desktop, Downloads or
    iCloud Drive, one Files and Folders prompt while onboarding's
    permissions page shows (Allow; for iCloud Drive, including Obsidian's
    iCloud folder, it reads "“Steno” wants to access files managed by
    “iCloud Drive”.", and may come at the first export instead, as R3
    records). Any other modal dialog fails R1.
- **R2 Handoff mechanics** (Forge).
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
- **R3 The real handoff** (**Nicolai**, a fresh account). The phone keeps one pairing, so pairing it here unpairs his
  daily install until he pairs it there again.
  1. Install `v0.10.0-rc.2`. Grant every permission, set an API key, choose
     a vault folder under `~/Documents`, pair the phone, record and process a
     meeting, and turn on launch at login. Save `sudo sfltool dumpbtm` as
     `btm-before.txt`.
  2. Quit. Record about 20 seconds on the phone while Steno is quit, and
     leave the recording queued there. Run `defaults write
     uno.schmid.steno.mac SUFeedURL http://localhost:8765/appcast.xml`
     (Sparkle 2.10 honours it in release builds), and relaunch.
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
     - the same page shows Speech models with the speaker models to download
       (about 33 MB): Download installs them with progress, and a meeting
       waiting with "Download the speech models in Settings" starts
       processing;
     - the same page shows the Obsidian vault row: macOS asks once for the
       Documents folder (Allow), and the Obsidian vault row reads Allowed;
     - the recording queued on the phone in step 2 uploads as exactly one
       new meeting, which is processed once the speech models are in, and
       the phone deletes its copy;
     - after that, a summary runs with the stored key and no prompt, and the
       phone uploads without pairing again;
     - a call records both lanes;
     - Settings shows the CoreML Parakeet as installed.

     Besides the prompts above, the Local Network prompt is expected once,
     when the new app starts listening for the phone (Allow). Any other
     modal dialog fails R3.

     **An iCloud vault**, when his daily vault is in Obsidian's iCloud
     folder (`~/Library/Mobile Documents/iCloud~md~obsidian/Documents/`):
     this account is signed in to iCloud with iCloud Drive on, and step 1
     chooses a vault in that folder instead of `~/Documents`. The vault
     row's prompt then reads "“Steno” wants to access files managed by
     “iCloud Drive”." with the app's reason below it (Allow), and Files &
     Folders lists iCloud Drive below the new app afterwards. Record when
     it came: at the vault row, or only at the first export (macOS may
     ask only when Steno first reads a file there). The pass list is
     otherwise the same.
  5. **The old login item.** Save `sudo sfltool dumpbtm` again as
     `btm-after.txt`. Log out and in: Steno starts once. Turn launch at login off
     in the new app, save `sudo sfltool dumpbtm` as `btm-off.txt`, log out and
     in: if Steno still starts, the old entry launches the new app. Record what
     Login Items shows; S6's handling follows from it. End the step with launch
     at login on again.
  6. Run `defaults delete uno.schmid.steno.mac SUFeedURL`.
- **R4 Updates after the handoff** (**Nicolai**, R3's account).
  1. Publish the next candidate, which changes only the version.
  2. Quit. Set `lastCheckAt` in `~/Library/Application
     Support/Steno/update-check.json` to 25 hours ago (an RFC 3339 UTC string
     such as `{"lastCheckAt":"2026-10-09T08:00:00Z"}`), and relaunch.

  Pass when:
  - the scheduled check finds the new candidate and offers or installs it;
  - after the relaunch, `ps -o command= -p $(pgrep -u "$(id -u)" -x
    steno-desktop)` shows `~/Applications/Steno.app/Contents/MacOS/steno-desktop`
    and About shows the new version;
  - no keychain or TCC prompt appears again;
  - with the speech setting on the ONNX engine for one meeting,
    `steno-speech-sidecar` starts from the same bundle (switch it back).
- **R5 Fresh install** (**Nicolai**, a fresh account; the soak on Forge).
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
     <steno>`) stays under A1's soak bound of 6 GiB, and the transcripts are complete.
  6. A stalled recording (A18). Record a call; a minute in, stop the app's
     process three times for 0.6 s, a few seconds apart, in Terminal:
     `p=$(pgrep -x steno-desktop); for i in 1 2 3; do kill -STOP $p; sleep 0.6;
     kill -CONT $p; sleep 3; done`. Record another minute and stop. Pass when
     the note after the stop says "About 2 seconds of the recording are
     missing." and the meeting transcribes as usual. A heavy load in another
     app does not do this reliably: the IO thread runs at real-time priority.
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
     (`security find-generic-password -s uno.schmid.steno.mac -a handover-identity`),
     whether `~/Library/Application Support/Steno/preferences.json` holds
     `steno.swiftImportRan`, and whether the import's own entry exists
     (`security find-generic-password -s uno.schmid.steno.mac -a swift-import-done`).
     If any of the three is found, the dogfood stops until Codex has
     resolved it: the marker or the flag would skip the import, and a
     desktop-id identity would be served to a phone that does not know it.
     No desktop-id install in this account is updated before the dogfood.
  3. His daily install takes the candidate through the local feed, following
     "Nicolai's Mac" below, and the `SUFeedURL` default is deleted afterwards.

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

### Nicolai's Mac

The dogfood's migration in his own account, on the day he takes
`0.11.0-rc.N` (D9: no Swift bridge release ships; he is the only user, so
whatever makes this migration not seamless is a step here or a fix before
the candidate). R1 to R6 pass first, in fresh accounts: R3 is the only
end-to-end proof of the keychain export and of the iPhone against the new
app. The phone is then paired with his daily install again (dogfood step 1;
pairing again moves `pairedAt` forward, which the import accepts).

Every step is **Nicolai**'s, in order, with Codex beside him where a step
names it. He needs `gh` signed in (`gh auth status`) and `python3`, both
for step 11. Before step 11, every command reads his data, or copies it
into `~/steno-backup`, and the backup (step 3) comes before anything that
writes. The only other writes are the `SUFeedURL` and
`SUAutomaticallyUpdate` defaults and the installs themselves. Nothing here
deletes or moves his data, apart from step 32, the rollback's last resort,
which moves the current folder aside and deletes nothing. `~/steno-backup`,
the `v0.10.0-rc.2` disk image and the Swift keychain items ("Steno handover
identity", `llm-api-key`) stay for the 14 days, since a rollback reads them,
and neither app is paired again unless Settings > iPhone asks for it.

A check that does not come out as its step says: stop, unless the step
says what to do instead. Codex resolves every stop with him before he
goes on.

Each command is one line for Terminal's default shell, zsh: paste one line
at a time, with `N` and anything in angle brackets filled in first. What it
should print is in the step's text.

**Before the handoff**

1. **Every Steno on the Mac.**

   ```zsh
   mdfind 'kMDItemCFBundleIdentifier == "uno.schmid.steno.mac"'
   defaults read /Applications/Steno.app/Contents/Info.plist CFBundleShortVersionString
   defaults read /Applications/Steno.app/Contents/Info.plist CFBundleVersion
   ```

   The first lists only `/Applications/Steno.app`, the second prints
   `0.10.0-rc.2` and the third `542`. Any other copy (`~/Applications`, a
   mounted Steno disk image, a build folder, `/nix/store` from the flake):
   stop. A Homebrew cask copy in `/Applications` is fine.
2. **No earlier build of the new app ran in this account.**

   ```zsh
   security find-generic-password -s uno.schmid.steno.mac -a swift-import-done
   security find-generic-password -s uno.schmid.steno.mac -a handover-identity
   ls "$HOME/Library/Application Support/Steno/preferences.json"
   ls "$HOME/Library/Application Support/Steno/handover-identity.json"
   ls "$HOME/Library/LaunchAgents/Steno.plist"
   ```

   Each answers "could not be found" or "No such file or directory". If any
   exists: stop (dogfood step 2).
3. **Back up, with Steno quit** (menu bar > Quit Steno).

   ```zsh
   mkdir "$HOME/steno-backup"
   ditto "$HOME/Library/Application Support/Steno" "$HOME/steno-backup/Steno"
   defaults export uno.schmid.steno.mac "$HOME/steno-backup/defaults.plist"
   ditto -c -k --keepParent /Applications/Steno.app "$HOME/steno-backup/Steno-0.10.0-rc.2.zip"
   ```

   Each prints nothing. If `mkdir` says "File exists", an earlier run left
   a backup there, and copying over it would mix two databases: stop. Keep
   the `v0.10.0-rc.2` disk image too.
4. **The pairing data.** The queries read a copy of the backup's database,
   so sqlite3 opens neither the live database nor the backup, which stays
   as it was for a rollback.
   - Copy the backup's database files into a scratch folder.
     `steno.sqlite*` always matches the database itself, and its `-wal`
     and `-shm` files when they are there:

     ```zsh
     mkdir "$HOME/steno-backup/query"
     cp "$HOME/steno-backup/Steno/"steno.sqlite* "$HOME/steno-backup/query/"
     ```

     If `mkdir` says "File exists": stop, since an older `-wal` file there
     would show an older database.

   - The paired phones: one row, his iPhone.

     ```zsh
     sqlite3 "$HOME/steno-backup/query/steno.sqlite" "select id, name, pairedAt from pairedDevice"
     ```

   - The certificates: it prints `1`.

     ```zsh
     security find-certificate -a -c "Steno on" -Z | grep -c "SHA-1 hash"
     ```

   - The certificate's date: the `notBefore` it prints (one date, for the
     one certificate) is before that `pairedAt`.

     ```zsh
     security find-certificate -a -c "Steno on" -p | openssl x509 -noout -startdate
     ```

   - System Settings > General > Date & Time: Set time and date
     automatically is on. If it is off, turn it on.

   Anything but one row and `1`: stop.
5. **The settings to compare later**, from step 4's copy:

   ```zsh
   sqlite3 "$HOME/steno-backup/query/steno.sqlite" "select key, value from setting" > "$HOME/steno-backup/settings-before.txt"
   ```

   In `settings-before.txt`, note the vault path (the `obsidian` row):
   under Documents, Desktop, Downloads or iCloud Drive, step 18's prompt
   comes. A vault in Obsidian's iCloud folder
   (`~/Library/Mobile Documents/iCloud~md~obsidian/...`) is in iCloud
   Drive: step 18 says which prompt to expect. Note whether the
   `defaultRetention` row says `keepForever`; step 30 depends on it.
6. **The settings load.** Codex runs, from a checkout of the candidate's
   tag:

   ```zsh
   scripts/verify-store-against-swift-db.sh "$HOME/steno-backup/Steno/steno.sqlite"
   ```

   It opens only a copy of the copy and prints the migrations and a
   `settings: provider ..., retention ...` line with no error. If it prints
   `error: the stored setting <key> is not in a form Steno can read` or
   `error: the stored setting <key> holds a value this version of Steno does not understand`,
   the candidate would refuse to start, and its dialog would name that
   setting and the database: stop.
7. **The speech model.**

   ```zsh
   stat -f %z "$HOME/Library/Application Support/Steno/Models/fluidaudio/parakeet-tdt-0.6b-v3/Encoder.mlmodelc/weights/weight.bin"
   ```

   It prints `445187200`. Anything else means one download of about 483 MB
   later; step 17's row then offers it too.
8. **The network.**
   - Mac and phone are on the same Wi-Fi.
   - The firewall:

     ```zsh
     /usr/libexec/ApplicationFirewall/socketfilterfw --getglobalstate
     /usr/libexec/ApplicationFirewall/socketfilterfw --getallowsigned
     ```

     The firewall is off, or both "allow signed" lines end in `ENABLED`.
     Else, in System Settings > Network > Firewall > Options, turn on
     "Automatically allow built-in software to receive incoming
     connections" and "Automatically allow downloaded signed software to
     receive incoming connections", then run both commands again.
9. **The phone uploads to the Swift app today.**
   - Open Steno again.
   - Record 10 seconds on the phone and see it arrive on the Mac.
   - Turn the phone's Wi-Fi off, record 20 seconds more and leave it
     queued: step 19's recording.
10. **Nothing in flight.** Wait until nothing is recording and no meeting
    reads Queued or Processing in the list (step 9's 10 seconds included).

**The handoff**

11. **The candidate's feed** (as in R3), in `~/steno-backup/feed`.
    - The folder:

      ```zsh
      mkdir -p "$HOME/steno-backup/feed"
      cd "$HOME/steno-backup/feed"
      ```

    - The disk image:

      ```zsh
      gh release download v0.11.0-rc.N -R NicolaiSchmid/steno -p 'Steno_*_aarch64.dmg'
      ```

    - The item, from the tag's release run: the first command lists the
      run, and its ID goes into the second.

      ```zsh
      gh run list -R NicolaiSchmid/steno --workflow desktop-release.yml --branch v0.11.0-rc.N
      gh run download <run id> -R NicolaiSchmid/steno -n sparkle-item
      ```

      The folder then holds `appcast.xml` beside the disk image.
    - Codex rewrites the enclosure URL in `appcast.xml` to
      `http://localhost:8765/<dmg name>`.
    - In a second Terminal window, the server, which keeps that window
      busy:

      ```zsh
      cd "$HOME/steno-backup/feed"
      python3 -m http.server 8765 --bind 127.0.0.1
      ```

    - Back in the first window, this prints the feed:

      ```zsh
      curl -fsS http://localhost:8765/appcast.xml
      ```

12. **Install.**
    - Quit Steno.
    - Point the Swift app at the local feed:

      ```zsh
      defaults write uno.schmid.steno.mac SUFeedURL http://localhost:8765/appcast.xml
      ```

    - Open Steno, choose Check for Updates, then Install and Relaunch. The
      server's window shows `GET /appcast.xml` and `GET /<dmg>`.
    - If Sparkle refuses: quit Steno, open the disk image, drag Steno onto
      Applications and choose Replace, then open it and choose Open at the
      Gatekeeper question.
13. **The feed goes.**
    - Remove the default:

      ```zsh
      defaults delete uno.schmid.steno.mac SUFeedURL
      ```

    - Stop the server with Ctrl-C in its window.

**The first launch**

14. **Steno comes back by itself** (within a minute; else open
    `/Applications/Steno.app`). Every meeting is listed, and before the next
    step no keychain prompt has come.

    ```zsh
    defaults read /Applications/Steno.app/Contents/Info.plist CFBundleIdentifier
    lsof -nP -a -c steno-desktop -iTCP -sTCP:LISTEN
    ```

    The first prints `com.nicolaischmid.steno.desktop`; the second, run
    before the next step, prints nothing.
15. **The import step comes first.** Choose Continue, never Not now. macOS
    asks for the login keychain password for `llm-api-key`: type your login
    password and choose Always Allow. Then "steno-desktop wants to export
    key 'Steno handover identity'": type your login password and choose
    Allow (Always Allow if offered). With ChatGPT summaries and no API key,
    only the second prompt comes. If either was denied, choose Try again in
    Settings > iPhone, never Pair again.
16. **Local Network and the permissions page.** Each prompt comes once.
    - Local Network comes first: right after the import step, the new app
      starts listening for the phone, and macOS asks "Allow Steno to find
      devices on local networks?", before or during the prompts below:
      Allow.
    - Microphone: Allow.
    - System audio: choose Run the test recording; it plays a tone for up
      to 30 seconds; allow the audio recording prompt.
    - Calendar: allow full access.
    - System Settings > Privacy & Security > Local Network shows Steno
      switched on (two "Steno" rows are expected; the other is the old
      app). If the Local Network prompt was missed, switch Steno on there
      (Settings > iPhone says where), then quit and reopen Steno.
17. **Speech models**, the row on the same page. It offers the speaker
    models (about 33 MB): choose Download. It ends on Installed. Settings >
    Transcription shows the Parakeet v3 model and the Speaker diarization
    model as Installed, and any meeting showing "Download the speech models
    in Settings" starts processing.
18. **The Obsidian vault**, the row on the same page when a vault is set.
    With the vault under Documents, Desktop, Downloads or iCloud Drive,
    macOS asks for that folder while the page shows: Allow. The row reads
    Allowed.
    - A vault in Obsidian's iCloud folder is in iCloud Drive: macOS 26
      asks "“Steno” wants to access files managed by “iCloud Drive”."
      instead, with Steno's reason below it: Allow. If no prompt comes
      and the row reads Allowed, go on, but if one comes later (at step
      21's Export again), Allow it and tell Codex.
    - If it says "Steno cannot write your meeting notes to <vault>": choose
      Open System Settings. Under Files & Folders, Steno is a heading with
      one switch per place below it (Documents Folder, Desktop Folder,
      Downloads Folder, iCloud Drive, Removable Volumes, Network Volumes):
      turn on the one the vault is in, then choose Check again. If two
      Steno headings show, the new app's is the one with that switch off.
      Check again writes to the vault again; the page stays until it has.
    - If it says "Steno cannot use <vault> for your meeting notes" (the
      folder is not there, for example an unmounted disk): stop.
19. **The phone.** Turn its Wi-Fi back on and open Steno on it. The
    recording from step 9 uploads without pairing again: the Mac lists
    exactly one new phone meeting and processes it, and the phone deletes
    its copy. On the Mac, this shows the Mac's name (Ctrl-C to stop):

    ```zsh
    dns-sd -B _steno._tcp
    ```

    If the phone stays on "Waiting for Mac", check step 16's switch, then
    quit and reopen Steno on the Mac.
20. **Summaries.** That meeting gets its summary with no keychain prompt;
    with ChatGPT summaries, from the same model as before.
21. **The vault.** Edit one Steno note in Obsidian, then choose Export
    again on that meeting: no prompt comes, the note is in Obsidian, and
    your edited note stays, with a `(Steno <date>).md` beside it instead
    of being overwritten.
22. **A real call.** One short call with sound on both sides: both lanes are
    transcribed, and the call prompt appears for the call app.
23. **No second prompts.**
    - Quit Steno (menu bar > Quit Steno).
    - Copy the database files into `~/steno-backup` and compare that copy's
      settings with step 5's:

      ```zsh
      mkdir "$HOME/steno-backup/query-after"
      cp "$HOME/Library/Application Support/Steno/"steno.sqlite* "$HOME/steno-backup/query-after/"
      sqlite3 "$HOME/steno-backup/query-after/steno.sqlite" "select key, value from setting" | diff "$HOME/steno-backup/settings-before.txt" -
      ```

      If `mkdir` says "File exists": stop, as in step 4. The values are
      the same. A row may differ only in how it is written
      (key order inside an object, or a number's form), apart from the
      speech engine if it was Whisper, Ultra or the German model, and an
      added `handoverPort`.
    - Open Steno: no keychain and no permission prompt.
24. **The login item.**
    - Settings > General: Open Steno at login is on.
    - System Settings > General > Login Items & Extensions, under Open at
      Login, shows one Steno. If there are two, this shows which belongs to
      `uno.schmid.steno.mac`; it stays until the 14 days end (a rollback
      needs it):

      ```zsh
      sudo sfltool dumpbtm | grep -B4 -A12 -i steno
      ```

    - Log out and in: Steno starts once. The first command shows one
      process, the second says "No such file or directory":

      ```zsh
      pgrep -lx steno-desktop
      ls "$HOME/Library/LaunchAgents/Steno.plist"
      ```

**The 14 days after**

25. **Each day of the 14.** The meetings count towards G2. No crash log:
    each of these prints nothing.

    ```zsh
    ls "$HOME/Library/Application Support/Steno" | grep crash-
    ls "$HOME/Library/Logs/DiagnosticReports" | grep -i steno
    ```

26. **The tidy-up.** After the 14 days, Codex tidies up with him as a
    task of its own (the old login item, the Whisper and German models,
    the backup); nothing in this list removes them.

**The rollback, only if needed**

Roll back when a stop after step 14 cannot be fixed forward the same day
and he needs to record; a fix forward (a new candidate) is the rule
otherwise.

27. **A copy of the new app.** Quit Steno. If
    `~/steno-backup/Steno-0.11.0-rc.N.zip` exists already, skip this step:
    a second run would replace it, or zip the Swift app under that name
    after step 28. Else:

    ```zsh
    ditto -c -k --keepParent /Applications/Steno.app "$HOME/steno-backup/Steno-0.11.0-rc.N.zip"
    ```

28. **The Swift app back.**
    - Drag Steno from the `v0.10.0-rc.2` disk image onto Applications and
      choose Replace.
    - Before opening it, this says the default does not exist:

      ```zsh
      defaults read uno.schmid.steno.mac SUFeedURL
      ```

    - Turn its automatic updates off:

      ```zsh
      defaults write uno.schmid.steno.mac SUAutomaticallyUpdate -bool false
      ```

29. **The Swift app runs.** Open it. Every meeting is listed, the ones the
    new app recorded too; it records with its old grants, summarises with
    no prompt, and the phone uploads to it without pairing. If the new app
    paired again (Settings > iPhone asked for it), the phone cannot send
    to the Swift app ("Pair again in the Rust app, then a rollback"): stop.
30. **What not to do on the Swift app.** No Pair again. If retention is not Keep
    forever, also no Export again, speaker rename or summary re-run on a
    meeting the new app processed: the Swift app stamps its retention, and
    its sweep can then delete the recording ("Retention during a
    rollback").
31. **Back to the new app.**
    - Quit Steno.
    - Unpack the copy from step 27:

      ```zsh
      ditto -x -k "$HOME/steno-backup/Steno-0.11.0-rc.N.zip" "$HOME/steno-backup/rust-app/"
      ```

    - Drag `~/steno-backup/rust-app/Steno.app` onto Applications and choose
      Replace.
    - Open it: no import step, the meetings the Swift app received are
      listed, and the phone uploads without pairing.
32. **The last resort**, only if the database itself is in doubt.
    - Quit Steno.
    - Move the current folder aside:

      ```zsh
      mv "$HOME/Library/Application Support/Steno" "$HOME/steno-backup/Steno-current-$(date +%Y%m%d-%H%M%S)"
      ```

    - Copy the backup back:

      ```zsh
      ditto "$HOME/steno-backup/Steno" "$HOME/Library/Application Support/Steno"
      ```

    Recordings made since step 3 are then only in the folder moved aside,
    which stays.

### The Linux gates

Each runs on Nicolai's own machine, in a new user account, before the site lists
the target, from `0.11.0-rc.2` on (`rc.1` has no previous candidate). Step 0 on
each target installs the previous candidate (`rc.<N-1>`), opens the firewall
where there is one, pairs the phone, records a one-minute meeting and turns on
launch at login (on NixOS the module's `steno.service` does), then upgrades to
the candidate under test: `sudo apt install ./<new>.deb` on GNOME; `makepkg -si` in
`packaging/aur/` after the README's Bump steps 1 to 5 for the new tag (the
PKGBUILD at the tag still names the previous release) on Omarchy; the flake
input moved to the new tag, `nixos-rebuild switch` and `sudo
nix-collect-garbage -d` on NixOS. Step 0
passes when the meeting and the pairing are kept and, after a logout and login,
Steno starts once, as the new version. Each target's step 1 sets up its step 0;
steps 2 onward run on the candidate under test. On `v0.11.0`, R8 repeats step 0
from the last candidate and one recording. A4 still owes a measurement of
`start`'s first cycle and the capture latencies on the GNOME and Omarchy
machines; it runs with these gates.

Every target also runs one more step on the candidate, the stalled recording
(A18): `journalctl --user -b 0 | grep -i "without real-time scheduling"`
prints nothing, and `ps -L -o tid,cls,rtprio,comm -p $(pidof -s steno-desktop)
| grep data-loop` shows `FF` or `RR` (RealtimeKit installed and running; on
NixOS `security.rtkit.enable`); then record a call, stop the app's process
three times for 0.6 s a minute in (`p=$(pidof -s steno-desktop); for i in 1 2
3; do kill -STOP $p; sleep 0.6; kill -CONT $p; sleep 3; done`), record another
minute and stop. Pass when the note after the stop says "About 2 seconds of
the recording are missing." and the meeting transcribes as usual.

The simplify plan gives up the save at a Linux logout, reboot or OOM kill
("Given up"), so the gates no longer check the session-end save, the stop
timeouts, Steno's own systemd scopes or the kept autostart entry. A logout
that loses a recording is reported, not a gate failure.

The status queries used throughout (`sqlite3`, or `nix-shell -p sqlite` on
NixOS, for `q`):

```sh
q() { sqlite3 ~/.local/share/Steno/steno.sqlite \
  "select datetime(startedAt), state, round(duration,1), endReason, failureReason from meeting order by startedAt desc limit 3"; }
# The cgroup of every running Steno, one line each. `pidof` misses a Steno
# that an old autostart entry starts as `.steno-desktop-wrapped`.
cg() { for p in /proc/[0-9]*; do
  case "$(readlink "$p/exe" 2>/dev/null)" in
    */steno-desktop | */.steno-desktop-wrapped) cat "$p/cgroup" ;;
  esac
done; }
```

Good: the newest meeting is `queued` or later, with a duration near the time
recorded. After a `kill -9`, its `state` at the next launch is `queued` or
later with `failed` as `endReason` (P3's salvage). On the GNOME machine,
`systemd-coredump` is installed before the gate, so crashes show in
`coredumpctl list`.

- **GNOME** (Ubuntu 24.04 or Debian 13, named in the result).
  1. Step 0 verifies each `.deb` first with the README's `gpg --verify` and
     `sha256sum --check`.
  2. Onboarding shows no permission as missing; Settings downloads the speech
     model with progress; Settings > Recording lists the input devices (A7).
  3. The phone, paired in step 0, uploads a recording; `ss -ltn 'sport =
     :23820'` lists Steno's listener (X4).
  4. Join a Google Meet call in Firefox: the detection prompt appears (A8);
     record it, with the floating panel on top; stop; the meeting has a
     microphone lane and a system lane, and is transcribed, diarized and
     exported.
  5. Record for a few minutes, then `kill -9` the app: at the next launch the
     meeting is recovered with `failed` as `endReason` and processes (P3).
  6. Quit the `.deb`'s Steno; the AppImage starts (`pgrep -a steno-desktop`
     shows only its path) and finds the same meetings.
  7. **The tray (X3).** With the AppIndicator extension on (on by default on
     Ubuntu; on Debian install `gnome-shell-extension-appindicator`, log out
     and in, then `gnome-extensions enable <id>`, the id from
     `gnome-extensions list | grep -i appindicator`, or turn it on in the
     Extensions app): the icon is in the top bar, a left click and a right
     click each open the menu, and every entry works; closing the main window
     hides it, and Open Steno brings it back. Then turn the extension off
     (`gnome-extensions disable <id>`) while Steno runs: `journalctl --user -b
     0 --since '<disable time>' | grep -i steno` shows `no tray host shows the
     tray icon`. Start a recording and close the main window: Steno quits
     within 10 s, `q` shows the meeting with its full length and `quit` as
     `endReason`, and `journalctl --user -b 0 --since '<close time>' | grep -i
     steno` shows `the main window closed with no tray` before `the shutdown
     ended`. Turn the extension on again and start Steno: the journal shows
     `a tray host shows the tray icon`.
- **Omarchy** (Omarchy 4, Arch with Hyprland on Wayland).
  1. Before step 0: import the release key from a checkout (`gpg --import
     packaging/aur/keys/pgp/048B527950E4F609B90E63495F8810A6E6D4DB46.asc`), so
     the PKGBUILD verifies each `.deb`'s signature; add the README's Lua window
     rules to `~/.config/hypr/` and reload Hyprland. In step 0, `sudo ufw allow
     Steno` runs right after the install and before the pairing, and the upgrade
     runs while the previous candidate, started at login, records; then stop the
     recording: the meeting processes. From `v0.11.0` on, step 0 installs with
     `yay -S steno-desktop-bin`, which verifies with the imported key (and
     imports it itself once it is on keyserver.ubuntu.com).
  2. Start Steno from the Omarchy launcher. `cat /proc/$(pidof -s
     steno-desktop)/cgroup` names an `app-…scope` (not Hyprland's unit). The
     tray icon is in the bar's drawer; a right click opens the menu, and every
     entry works; closing the main window hides it, and Open Steno brings it
     back (X3).
  3. Start a recording: `hyprctl clients -j | jq '.[]|select(.class |
     test("steno-desktop"; "i"))|{title,xwayland,floating,pinned}'` shows "Steno
     bubble" as `xwayland: true`, floating and pinned (under XWayland the class
     is `Steno-desktop`); it did not take focus (`hyprctl activewindow`); it
     stays on a workspace switch; a drag moves it, and the place survives a
     restart (X2).
  4. Secrets: save a password in Chromium, set an API key, then reboot (the
     pairing from step 0 stands). Pass when no keyring prompt appears at boot;
     `journalctl -b | grep -i 'unrecognized format'` is empty; `secret-tool
     search --all service uno.schmid.steno.mac` lists `llm-api-key` and
     `handover-identity`; Chromium's saved passwords still open (P8).
  5. The phone uploads a recording through the firewall (X4): `sudo ufw
     status` lists `Steno` as `ALLOW` and no wider rule, `ss -ltn 'sport =
     :23820'` lists Steno's listener, and `journalctl --user -b | grep
     'is not available'` prints nothing.
  6. Steps 4 and 5 of the GNOME gate, with the call in Chromium.
  7. In the Steno that a login started, Settings says updates come from the
     package manager (X5), and `~/.config/autostart/steno-desktop.desktop`'s
     `Exec` names `/usr/bin/steno-desktop`.
- **NixOS** (x86_64-linux, GNOME, and Hyprland with `withUWSM = true` if
  Nicolai runs it).
  1. Step 0 uses a flake input at the previous candidate's tag
     (`github:NicolaiSchmid/steno/v0.11.0-rc.<N-1>`), with
     `steno.nixosModules.default` in the system's modules and
     `programs.steno.enable = true`.
  2. Steps 2 to 5 and 7 of the GNOME gate, with the phone through the
     firewall the module opens (X4), and step 7's extension from
     `gnomeExtensions.appindicator` in `environment.systemPackages`, turned on
     in GNOME's Extensions app. `systemctl --user show steno.service -p
     Environment` names `STENO_HANDOVER_PORT=23820`, and `cg` prints one line,
     ending in `steno.service`.
  3. Open Settings and choose a folder: the file chooser opens (the wrapper's
     schemas).
  4. Launch at login: after step 0's upgrade and collection, `systemctl --user
     cat steno.service` names the profile path, and About shows the new version.
  5. Settings says updates come from the package manager.
  6. A switch that restarts PipeWire, as most nixpkgs bumps do: start a
     recording, add `systemd.user.services.pipewire.environment.STENO_GATE =
     "1";` to the system, `nixos-rebuild switch`, wait a minute, remove the
     line, switch again, wait a minute and stop. `systemctl --user show
     pipewire.service -p ActiveEnterTimestamp` is the time of the second
     switch. Pass when `q` shows one meeting for the recording, `queued` or
     later, with a duration near the time recorded and `manual` as `endReason`
     (not `deviceLost`), and the audio has at most a couple of seconds of
     silence at each switch.
  7. An autostart entry from a build without X5: write
     `~/.config/autostart/steno-desktop.desktop` with `[Desktop Entry]`,
     `Type=Application`, `Name=Steno` and `Exec=<dir>/.steno-desktop-wrapped`,
     where `<dir>` is `dirname $(readlink -f
     /run/current-system/sw/bin/steno-desktop)`, then log out and in. Both
     units start Steno; the one `steno.service` starts wins and removes the
     entry at launch, and the other ends before setup. Pass when `cg` prints
     one line, ending in `steno.service`; `systemctl --user is-active
     'app-steno\x2ddesktop@autostart.service'` does not print `active`; and
     the entry is gone. If `cg` names the autostart unit instead, that
     instance won the start: log out and in once more, and check again.
  8. A package-only install: set `programs.steno.enable = false`,
     `nixos-rebuild switch`, and log out and in so the module's Steno is gone;
     `nix profile install
     github:NicolaiSchmid/steno/v0.11.0-rc.<N-1>#steno`, launch at login on;
     replace it with the candidate under test (`nix profile remove steno`, then
     `nix profile install github:NicolaiSchmid/steno/v0.11.0-rc.N#steno`);
     `nix-collect-garbage -d` as that user; log out and in: Steno starts once,
     as the new version, and its file chooser opens.

**The Windows gate** (before the site lists Windows): a Windows machine runs the
`--ignored` WASAPI tests, one real call, a logoff during a recording that saves
it, and a `kill` that P3 recovers. It installs an update from each Windows
installer (the MSI and the NSIS setup), from a build one version behind the
candidate: the new version starts. The P25 watcher's steps (a declined consent
prompt, a failed or concurrent `msiexec`, `DisableCMD`) are dropped: simplify
step 5 moves Windows updates to the Tauri updater's default behaviour, and a
declined prompt no longer restarts the old version.

**The stalled recording on Windows** (A18), with `RUST_LOG=steno_audio=info`, on four
pairs of default devices (Sound settings, each device's Properties,
Advanced): microphone and output at 48 kHz; both at 44.1 kHz; a
Bluetooth headset in hands-free mode (16 or 8 kHz: the headset's
"Hands-Free" endpoint as the microphone, during a call); and a device
at 96 kHz or above. Each time, record a call with nothing playing for a
minute, then play something: the log at the stop shows no "packets
after a glitch" line. Then record a call while something plays, and
suspend the app's process three times for 0.6 s and once for 1.5 s, a
few seconds apart (Sysinternals' `pssuspend steno-desktop`,
`Start-Sleep -Milliseconds 600`, `pssuspend -r steno-desktop`). Write
down, from the log, which loopback the system lane ran on ("system lane
on Process loopback" or "Endpoint loopback" at the start) and both
streams' "device positions advanced" lines at the stop. Pass on those
lines:
- Every line shows its QPC, near 1.0000 a frame (within 2 %): a line
  that says "QPC no reading", or a QPC ratio further off, fails the step,
  so a QPC missing on real hardware shows up here.
- Each says "jumps filled", the system stream on process loopback
  included: its QPC is sane even where its positions stand still. A line
  that says "jumps counted, not filled" at `warn` fails the step. Write down
  each line's device-position ratio too (1.0000, or the engine's rate
  over 48 000: 0.9188 at 44.1 kHz, 0.3333 at 16 kHz, 2.0000 at 96 kHz).
- A line that says "0.0000 a frame" fails the step only when the same
  stream has no sane QPC: it says "jumps counted, not filled" and shows
  no QPC ratio near 1.0000. Its device positions never moved, so its
  losses below the drift allowance are silent. Positions that stand
  still beside a sane QPC (seen on process loopback) pass.
- The note after the stop says "About 3 seconds of the recording are
  missing.", and each stream that filled has a "packets after a glitch"
  line of about 3 s missed.
- The "system stream staging" line shows at most 3 840 frames skipped
  (slipped) per suspension: the engine kept the oldest packets of each
  stall, as the fill expects, and the lanes stayed aligned (the
  microphone's thread woke first where it slipped). A recording that was
  not suspended and still shows about a second skipped had the
  microphone's thread alone stalled for 1.2 to 2.5 s: its note must count
  that time too.

Then record a call for a minute while something plays, pause the
playback for 10 s halfway, and play again. Do it on endpoint loopback,
which delivers nothing while nothing plays: Steno uses it only where
process loopback fails to start, so run it on a Windows build without
process loopback (an older Windows 10) or wherever the start says
"Endpoint loopback", and do it once on process loopback too. Pass: the
stop shows no "packets after a glitch" line and no note about missing
audio, and the system stream's "device positions advanced" line passes
as above (the idle left out of both measurements). Write down which loopback
each run used; with no endpoint loopback machine, say so beside the
step.

Last, the stall just before the stop, once on each loopback: record a
call for a minute while something plays, suspend the app's process for
0.6 s as above, and stop the recording within a second of resuming it.
Then once more without the suspension. Pass: after the suspended run
the stop's `warn` line "the capture's thread missed ... s of audio"
reads about 0.6 s (the note stays silent for a second or less), and the
microphone's stream logs either a "packets after a glitch" line or a
"missed after its last packet" line of about 0.6 s; the system stream does the same on process loopback. On
endpoint loopback, write down whether the system stream logged either
line: when it logs neither, its engine refused the newest packets and
that loss on the system lane went uncounted (the lane reads no clock at
the stop there), which decides whether endpoint loopback needs a check
of its own. After the run without the suspension no stream logs a
"missed after its last packet" line.

Then the stall right after the start, once on each loopback, a new
recording each time: with something playing, start a call recording and
suspend the app's process for 0.6 s within half a second (run
`pssuspend steno-desktop` as the recording starts), then record a
minute. Then once more without the suspension. Pass: after the
suspended run the stop's `warn` line "the capture's thread missed ... s
of audio" reads about 0.6 s (the note stays silent for a second or
less), and the microphone's stream logs a "missed before its first
packet" or a "packets after a glitch" line of about 0.6 s (or 0.6 s
less what its engine kept); the system
stream does the same on process loopback. On endpoint loopback, write
down whether the system stream logged either line: it reads no start,
so a loss before its first packet there is uncounted. After the run
without the suspension no stream logs a "missed before its first
packet" line.

## Order of operations and gates

1. **Every decision is confirmed** (2026-10-07). Done.
2. **Every package is written.** Done: S1 to S7, A1 to A20, P1 to P40 and X1
   to X7 are merged (Closed packages). Any further gap the data-loss audit
   finds lands as a new candidate (G2).
3. **Gate G1.** Every S, A and P row and X1 to X7 are closed, and the audio
   path is final (D9). Met on 2026-10-18. The speaker-clip follow-up (Open
   work) is still open. X8 runs in G2.
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
   - **A10's call-mode checks** (**Nicolai**, on his Mac in a GUI session,
     from a checkout of the candidate's tag with its CLI built: `cargo build
     --release -p steno-cli`; the commands run from the checkout's root).
     Before every step: the terminal app has Privacy & Security, Screen &
     System Audio Recording and Microphone, turned on; Do Not Disturb is on,
     so no alert sound plays; no app plays audio; and the built-in microphone
     stays the input (System Settings, Sound, Input), since macOS moves the
     input to a headset as it connects. Steps 1 to 3 run on the Mac's
     speakers.
     1. Nothing playing: `target/release/steno dev capture-spike --lanes call
        --seconds 20 --out ~/a10`. Pass when it prints `nonzero system: 0 of N
        samples`, with N about 960000: digital silence.
     2. Both bounds, then the tone.
        - `cargo test --release -p steno-audio --test live
          call_capture_runs_from_its_start_with_nothing_playing -- --ignored
          --nocapture`. Pass when it passes and prints `first callback A after
          start returned (bound 100ms), B after start was called (bound
          200ms)` with A at most 100 ms and B at most 200 ms. When the one- or
          the five-minute load is 4 or more it prints `end-to-end bound not
          checked: load N (1 min), M (5 min)`; B is then read against 200 ms
          by eye.
        - The tone: with the candidate in `~/Applications`, quit every Steno,
          the Swift app included, run `: > ~/a10.log` and `open --env
          RUST_LOG=info --stderr ~/a10.log ~/Applications/Steno.app`, start a
          call recording, play a short tone at 10 s (`sleep 10; afplay
          /System/Library/Sounds/Glass.aiff`), and stop 5 s later. Pass when
          `grep "first callback" ~/a10.log` prints `the capture's first
          callback (system tap) came N ms after its start` with N at most 100,
          and `target/release/steno dev onsets <the meeting's recording.caf>`
          puts the tone's onset in the system channel and its residual in the
          echo-cancelled mic channel within 50 ms (if the cancellation hides
          the residual, a second take with `target/release/steno record
          --mode call --keep-raw-mic --out ~/a10-raw` keeps the raw mic as
          `mic.raw.caf`).
     3. A hands-free headset: pair a Bluetooth headset as the output, keep the
        built-in microphone as the input, and run step 1's command with
        `--seconds 60`. While it records, in a second Terminal window, read
        the headset's "Current SampleRate" with `system_profiler
        SPAudioDataType | grep -A8 "<the headset's name>"`. Pass when the
        headset stays at 44.1 or 48 kHz. A headset that drops to hands-free
        because recording the call also opens its microphone is A11's known
        limit, noted (step 6), not a failure here.
     4. Alerts on the speakers, the output on a headset: in System Settings,
        Sound, set "Play sound effects through" to the Mac's speakers and the
        output to a headset, then run step 2's `cargo test` command with
        nothing playing. Pass on both bounds, as in step 2.
     5. The output switched mid-call, with "Play sound effects through" on
        "Selected sound output device": `RUST_LOG=info target/release/steno
        dev capture-spike --lanes call --seconds 60 --out ~/a10-switch 2>
        ~/a10-switch.log`; at about 20 s switch the output from the speakers
        to a headset (or AirPlay), at about 40 s back, and speak now and then.
        Pass when `grep -E "device|silent output" ~/a10-switch.log` shows, for
        each switch, `device change: DefaultOutputChanged, reconnecting`, then
        `device resumed: attempt 1, gap G s` with G under 1, then `the silent
        output runs on <the new device>`, and no `did not start` line; when
        the `mic ... dBFS  system ... dBFS` lines go on after each `device
        resumed`; and when the summary prints `duration` within 1 s of 60.
     6. AirPods, A11's known limit (noted, neither passed nor failed): with
        AirPods connected as the output and the built-in microphone chosen as
        the input, record a two-minute call in the app while a call or a
        video plays through the AirPods. Note whether the AirPods switch to
        hands-free while it records (their sound turns narrow, and
        `system_profiler SPAudioDataType | grep -A8 AirPods` shows a "Current
        SampleRate" of 16 or 24 kHz) and whether they switch back after the
        stop. The note goes on A11's line.
   - **A14's phone check** (**Nicolai**, three times, with a build of the
     phone app): start a recording on the phone, force-quit the app from the
     app switcher, relaunch it, let the recording upload, and check that the
     meeting on the Mac is Ready with the cut-off warning and as long as the
     recording ran (within a second or two). Once after about 30 s and once
     after more than 5 minutes, on the phone's built-in microphone; and once
     after about 30 s while recording through AirPods. If one fails or its
     length is off, the uploaded file (in the meeting's folder) is what the
     salvage needs to read; send its first 64 KB.
   - **A15's check** (**Nicolai**, on the Mac): a debug build of the
     candidate (`cargo build -p steno-desktop -p steno-speech-sidecar`, as
     the desktop README's "Run" says) transcribes several real meetings with
     CoreML, one over an hour among them, in one session. Pass when no
     meeting fails with "CoreML raised", the app never quits, and
     `footprint -p steno-desktop` taken after each meeting stays within
     about 100 MB of the value after the first, with the IOSurface row
     under 200 regions.
   - **P35's live Keychain check** (**Nicolai**, a spare macOS account with
     a paired phone): the case #282 tests over the secrets file, run against
     the real Keychain. A login keychain that comes back empty (Create New
     Keychain after a password reset), or holding another identity, makes
     Steno mint nothing: the listener stays off and Settings > iPhone waits
     with Try again and Pair again.
   - A19's forced-fallback check is dropped with the warm-up mark (simplify
     step 4).
   - A fix becomes a new candidate. The steps it touches run again, and a fix to
     the audio path or a new P package restarts the R5 soak and the stability
     count.
6. **Before the stable tag, Nicolai:**
   - creates the `appcast` environment, with himself as required reviewer,
     "Prevent self-review" off and deployment tags `v*`; `gh api
     repos/NicolaiSchmid/steno/environments/appcast --jq
     '[.protection_rules[].type]'` includes `required_reviewers` and
     `branch_policy`;
   - has his AUR account ready for the first push, and the release key on
     keyserver.ubuntu.com, so `yay -S steno-desktop-bin` verifies without a
     checkout (X6).
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

**Retention during a rollback.** P14's guard is Rust only. Swift's `redeliver`
and `resummarize` call `stampDeferredRetention` once every delivery succeeded, so
an Export again, a speaker rename or a summary re-run in the Swift app stamps a
meeting the Rust app kept unstamped (a diarizer fallback, an empty lane, a
damaged recording), and Swift's sweep can then delete its recording. Swift does
not read `damaged-audio.json`, so it stamps a damaged recording like any other.
Swift shows the room speaker the Rust app stores as an ordinary speaker. Swift is
not changed for this. The stamps that rc builds before #241 wrote on fallback meetings also
still expire, in either app: nothing clears them.

**The database.** Migrations only add (Tags and versions), v5 lands before the
first candidate, every migration until S9 is mirrored in `Migrations.swift`, and
none merges in the window. The last Swift release and every Rust build from P2
on open a newer database, so going back stays possible until the Swift app is
removed.

**Pair again in the Rust app, then a rollback.** A Pair again in the Rust app,
phones paired over the new identity, then a rollback to `v0.10.0-rc.2` leaves
those phones listed in the Swift app, which still serves the old identity they
no longer know. Nothing is lost: the Rust app's identity, the one the phones
pinned, stays in its keychain entry, and going forward again reconnects them.
But rc.2 cannot tell, so it shows the phones as paired while they cannot send.

**The Swift app's retention sweep** does not check an asset again before it
removes its files, a race #228 closed in Rust only. It stays moot after a
rollback: the race needs a meeting processed again, and the Swift app refuses
"Process again" (P9). A Swift build in the window also treats a master it cannot
look at (in a folder it cannot search) as gone again, as before #313, so its
"Keep all" leaves that asset's stamp on and a later sweep can delete audio the
user chose to keep.
