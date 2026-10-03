# Rust core and Tauri shell: Steno on macOS, Linux and Windows

Status: started 2026-10-02 on branch `refactor/rust-workspace`. Amends
`.plans/2026-09-24-initial-scope.md` (removes "Windows, Linux" from the v1 non-goals for
the next major version and replaces the "Language / UI", "Core" and "Apps" rows of the
platform table) and `.plans/2026-09-29-macos-webview-ui.md` (its "no Electron, Tauri,
Node at runtime" decision held for the Swift host; the Tauri shell replaces that host
at cutover). The evidence is `.plans/2026-10-01-cross-platform-spikes.md` and the
speech-stack decisions and gates in `.plans/2026-10-01-cross-platform-speech-stack.md`,
which this plan executes rather than restates.

## Goal

One product on three platforms from one codebase: a Rust core, a Tauri 2 shell, and
the existing React web UI unchanged except for its transport. The Mac keeps CoreML
speech on the Neural Engine; Linux and Windows run our fp32 ONNX export of the same
model. The Swift app keeps shipping until the Rust app reaches parity on the Mac and
reads the same database, so the cutover is a download, not a migration.

Not in this plan: GPU execution providers (speech-stack WP4, gate G4), the iOS
recorder (unchanged), and any new product feature. The diarization rebuild
(speech-stack WP3, gate G3) was outside it at the start and moved in on 2026-10-02 as
WP4d, so it ships with the Rust pipeline.
Feature work continues on the Swift app until cutover; anything merged there after
this plan starts is a parity item for the Rust side, tracked in the parity list below.

## Invariants

1. **The web UI does not know which host it talks to.** The bridge contract
   (`Sources/StenoBridge`, fixtures in `apps/macos/web/fixtures/bridge/`) is the
   oracle. The Rust bridge crate must decode every fixture and re-encode it
   byte-identically under the same JSON convention (sorted keys, ISO dates). A Tauri
   transport joins the WebKit and mock transports behind `BridgeTransport` in
   `apps/macos/web/src/bridge/transport.ts`; nothing above it changes.
2. **Same SQLite file.** The Rust store replays the GRDB migrations in
   `Sources/StenoCore/Storage/Migrations.swift` as SQL and proves it with a schema
   diff against a database the Swift CLI created. New migrations after this plan
   starts are written once in SQL and mirrored in `Migrations.swift` and the Rust
   `.sql` files until cutover; the parity test proves them equal.
3. **Audio never leaves the device.** Only `Destination` implementations and the LLM
   client open network connections, and the LLM client sends text.
4. **One speech pipeline above the tensors.** Chunker, overlap merge and the TDT decode
   loop are shared; the backends are CoreML (`objc2-core-ml`) on the Mac and ONNX
   Runtime (`ort`) elsewhere. ONNX inference runs in a sidecar process; the Mac stays
   one process.
5. **No allocation and no lock on the audio thread**, proven by the counting allocator
   in `crates/steno-audio/src/testing/rt.rs` in a test build.
6. **Scope stays the scope.** No new features in the port; a Rust behaviour that
   differs from Swift is a bug unless a plan says otherwise.

## Architecture

```
Cargo.toml                 workspace
crates/
  steno-core/              domain types, protocols (the pluggable boundaries), SQLite store and migrations, pipeline orchestration, settings
  steno-bridge/            the JSON contract (topics, methods, snapshots, params, envelope), fixture tests
  steno-host/              the view models behind the three windows, the bridge host over the store, the shell-side service traits and their fakes
  steno-audio/             capture backends (CoreAudio taps, PipeWire, WASAPI), ring buffer, AEC, writer
  steno-speech/            VAD, chunker, merge, TDT decoder; CoreML and ONNX Runtime backends; model store
  steno-diarize/           speaker diarization: segmentation and embedding backends (CoreML, ONNX Runtime), Steno's clustering and refinement, model store
  steno-llm/               OpenAI-compatible and Codex clients, cleanup and summary passes
  steno-adapters/          destinations (Obsidian, Markdown folder), export
  steno-handover/          phone handover server (TLS pinned), shared wire contract with mobile/
  steno-cli/               `steno` binary: record, process, export, dev tools
apps/
  desktop/                 Tauri 2 shell: windows, tray, panels, autostart, updater, keyring; bridge host
  web/                     the React app (moved from apps/macos/web once the Tauri shell hosts it)
  macos/                   the Swift app, unchanged until cutover, then removed
spikes/                    frozen evidence; code moves into crates and is deleted here as it lands
```

Dependency direction is the Swift one: `steno-core` depends on nothing of ours; every
other crate depends on `steno-core`; the shell and the CLI wire them. The shell holds
no logic: view models and the bridge host live in `steno-host` so the CLI, tests and
the shell share them, which is the extraction the spikes plan asked for in Swift and
which happens in Rust instead. (This plan first put them in a host module of
`steno-core`; the bridge depends on the core and the host needs both, so the host sits
above the two in its own crate, WP6a.) Everything the Swift app reached through a
system framework or a package the port has not written yet (permissions, login item,
updater, the recorder, the pipeline, speech models, the LLM probe, vault validation,
the handover listener, devices, files, the Finder) is a trait in `steno_host::services`
with a fake in `steno_host::fakes`, so the whole host runs without a shell on a temporary
database; the core's own boundaries (`steno_core::protocols`) are used where one exists.
One temporary exception: from WP3 until WP6 the shell carries a fixture host behind its
`fixture-host` feature that answers the bridge from the recorded fixtures, so the UI
runs on every platform before the pipeline exists.

Platform backends behind traits, two implementations before generalising: `Capture`
(CoreAudio, PipeWire, WASAPI, synthetic), `SpeechBackend` (CoreML, ONNX Runtime, fake),
`SecretStore` (Keychain, Secret Service, DPAPI via the `keyring` crate), `Updater`
(Tauri updater on every platform; Sparkle retires at cutover).

## Transition

Parallel build. The Swift app ships from `main` throughout. The Rust app is usable on
Linux first (no Swift app competes there), then on the Mac once the parity list is
empty, then on Windows once its capture backend passes the capture tests. Cutover on
the Mac is a release that ships the Tauri app under the same bundle id, reading the
same database and settings, with Sparkle pointing at the last Swift build's appcast
entry for the handoff.

Parity list (kept at the end of this file): every user-visible behaviour of the Swift
app, ticked when the Rust app matches it on the Mac. Feature PRs on the Swift app add a
line here.

## Repository setup for three platforms

- Cargo workspace at the root, `rust-toolchain.toml` pinned to stable, `rustfmt`,
  `clippy -D warnings` and a `cargo check` on `rust-version` in CI. Shared dependency
  versions live in the root `[workspace.dependencies]`, crates inherit them.
  `cargo deny` for licences arrives in WP8 with the first Linux release, once the
  dependency tree is complete (the Tauri and `directories` trees bring MPL-2.0 crates
  that an allow list has to name; CC-BY attribution for Parakeet is a runtime notice,
  not a crate licence).
- `.github/workflows/rust-ci.yml`: `ubuntu-latest`, `windows-latest` and the macOS
  runner (`MACOS_RUNS_ON`, same variable as Swift CI) build and test the workspace;
  Apple-only crates compile on every OS with their backends behind `cfg`. Tauri
  bundling per OS in a later workflow once `apps/desktop` exists; `release.yml`
  grows a matrix when the Linux build ships.
- Linux build dependencies for Tauri (webkit2gtk, libayatana-appindicator, PipeWire
  headers) in one script, `scripts/setup-linux.sh`, used by CI and by the worktree
  bootstrap.
- `AGENTS.md` workspace table gains the Rust rows; Swift rows stay until cutover.
- Models are never committed (`.gitignore` already covers `*.onnx`, `*.mlmodelc`); the
  speech crate downloads them from a manifest with checksums.

## Work packages

Order is dependency order; packages on one line run in parallel. Stacking: the
workspace branch (`refactor/rust-workspace`, PR #151) sat on
`t3code/assess-linux-windows-webui` (PR #150, the spikes and speech-stack plans); WP1,
WP2 and WP3 were PRs off it (#153, #155, #156) and reached `main` in that order after
#150 and #151. PR #161 makes `steno-bridge` depend on `steno-core` and removes the
bridge's copies of the macro and codecs. Packages after WP3 branch from `main`.

- **WP1 workspace and bridge.** Cargo workspace, toolchain, CI matrix, `.gitignore`,
  `AGENTS.md`. `steno-bridge` with every topic, method, snapshot, params and envelope
  type as serde structs; a test decodes all 51 fixtures and re-encodes them
  byte-identically; a second test fails when a Swift fixture has no Rust type.
- **WP2 store.** `steno-core` domain types and the SQLite store (rusqlite, bundled
  SQLite), migrations as SQL files; schema parity test against
  `crates/steno-core/tests/fixtures/schema.swift.sql`, dumped from a database the
  Swift CLI created, regenerated by a script on macOS. Settings store. The protocols
  and their fakes followed as their own PR (#162).
  **WP3 Tauri shell on fixtures.** `apps/desktop` with the three windows served from
  the web build over a custom protocol, a Tauri transport in the web app, and a bridge
  host that serves the fixtures: the full UI runs on Linux and Windows before any
  pipeline exists. Tray, floating panels and deep links follow in the same package.
  **WP4 speech.** `steno-speech` from the spike code: VAD, chunker, merge, TDT
  decoder; CoreML backend with parity harness against FluidAudio on the calibration
  corpus; ONNX Runtime backend through `ort` with the logits split validated on FLEURS
  German (closes the open item from the speech-stack plan); sidecar process; model
  manifest and download. Gate: FLEURS numbers within 0.5 points of the spike F table.
  **WP4d diarization.** `steno-diarize`: speech-stack decision 6 and gate G3, moved
  here on 2026-10-02 so it ships with the Rust pipeline. Segmentation and embedding
  behind one backend trait (CoreML over FluidAudio's models on the Mac, ONNX Runtime
  elsewhere), Steno's clustering, timeline, mapping and refinement ported from
  `Sources/StenoSpeech/Diarization`, the G3 harness over the Forge corpus.
  **WP5 audio.** `steno-audio` from `spikes/capture-rs`: CoreAudio backend with
  device-change rebuild, synthetic backend, writer, AEC; capture tests from
  `Tests/StenoAudioTests` ported. PipeWire backend. WASAPI backend last.
- **WP6 pipeline and host.** In two PRs. **WP6a host** (`steno-host`): the view
  models and the bridge host over the real store, the service traits and fakes, the
  fixture parity suite and the ported view model tests; the parity list below is
  filled from it. **WP6b pipeline**: orchestration (`process`, retention, speaker
  matching, export), the CLI, the shell switched from its `fixture-host` feature to
  `steno-host` (the feature is removed), the real `Recorder` and `Pipeline` behind the
  host's traits. Parity: the Swift `steno export` of a calibration meeting equals the
  Rust one field for field.
- **WP7 LLM, adapters, handover.** Ports of `StenoLLM` (Codex and OpenAI-compatible),
  `StenoAdapters`, `StenoHandover` (rustls, the pinned trust evaluation, the shared
  `wire.ts` contract test). Lands as three PRs: WP7a LLM, WP7b adapters, WP7c handover.
- **WP8 shell completion and Linux release.** Autostart, updater, keyring, onboarding
  permissions per OS, installer bundles; `cargo deny` with a licence allow list in CI;
  `release.yml` matrix; first Linux build.
- **WP9 Mac cutover.** Parity list empty, same bundle id, Sparkle handoff, Swift app
  removed, web app moved to `apps/web`, Swift rows removed from `AGENTS.md`.
- **WP10 Windows.** WASAPI capture, DirectML provider (speech-stack G4), installer.

## Risks

- The spike decoder is validated above the joint only; the ONNX logits split is WP4's
  first test, not an assumption.
- FluidAudio parity needs about 1,500 more lines of heuristics plus a harness; the
  harness is the deliverable because FluidAudio changes them in most releases.
- Tauri floating panels are webviews; the recording bubble and detection prompt get
  re-tuned on the Mac.
- Swift on the Mac and Rust everywhere means two capture backends on the Mac during
  the transition; only the Swift one ships until WP9.
- Two shells and two cores for some months: the parity list and the fixture oracle are
  what keep them honest.

## Parity list

Every user-visible behaviour of the Swift app, one line each, ticked when the Rust side
matches it. `[x]` means the host crate (WP6a) covers the rule without a shell, through a
service trait where the Swift app reached a framework; the crate named on the line
still has to put the real implementation behind that trait, and the shell (WP6b, WP8)
still has to draw the window side. `[ ]` is not ported yet.

### Topics

- [x] `app`: version, the setup banner (while meetings exist, the configuration is
  incomplete and "Not now" was not pressed), the deep-link requests consumed by the
  publish that carried them. Difference: `phone` is filled from the handover service
  (the Swift main window left it nil).
- [x] `recording`: from the `Recorder` trait, at most 20 Hz; the levels and the
  auto-stop countdown arrive with WP5's recorder.
- [x] `progress`: one entry per queued or processing meeting, fed by
  `Host::apply_meeting_event` and the meeting list.
- [x] `meetings.list`: filters, tag filter, FTS query, counts before the tag filter and
  the query, day groups in the viewer's zone, speaker chips, the selection that survives
  updates and clears only when its meeting is gone, the first fill's selection.
- [x] `meeting.detail`: `null` without a selection; retention line, keep toggle,
  summary status rows, export footer, speaker rows, turns, tasks, templates, re-run and
  re-export guards. Difference: the speakers model's error rides on the detail's
  `error` (the page has one error line).
- [x] `settings.general`, `settings.recording`, `settings.transcription`,
  `settings.summaries`, `settings.export`, `settings.iphone`: with the sidebar
  subtitles, refreshed after every Settings command, store change and onboarding save.
- [x] `onboarding`: the two pages, the required steps gating Done, page 1 advancing
  on its own once every step is handled, page 2's saved lines and the exit.

### Methods

- [x] `page.ready` (publishes every topic once, in order), `page.layout` (validated
  and dropped).
- [x] `meetings.setFilter`, `meetings.setTagFilter`, `meetings.setQuery`,
  `meetings.select` (a meeting not listed yet waits for its row),
  `meetings.delete` (asks first through the shell's `confirm`; refuses a recording or
  processing meeting with the Swift messages).
- [x] `meeting.setTab`, `meeting.setTags` (trimmed, lower-cased, de-duplicated,
  sorted), `meeting.setTemplate` (stores and re-runs; unknown ids change nothing),
  `meeting.rerunSummary`, `meeting.reexport`, `meeting.setKeepAudio` (asks first when
  turning keep off would delete now), `meeting.deleteRecordingNow` (both apply the
  answer to the meeting asked about, even when the selection moved during the prompt),
  `meeting.saveNotes` (to the named meeting, selected or not),
  `meeting.revealRecording`, `meeting.revealExport` (through the `Opener` trait).
- [x] `speakers.options`, `speakers.select` (confirm, merge, create with an attendee's
  email, the own person as a no-op, one re-export per change), `speakers.play`,
  `speakers.stop` (through the `ClipPlayer` trait).
- [x] `recording.start` (the live row becomes the requested meeting; a start that fails
  re-reads the permissions), `recording.stop`, `recording.toggle`,
  `recording.keepGoing`, `recording.clearMessages`: through the `Recorder` trait,
  which WP5 implements.
- [x] `setup.dismissBanner`.
- [x] `settings.general.setLaunchAtLogin`, `.setDetectionEnabled`,
  `.setDefaultTemplate`, `.requestCalendar`, `.setAutomaticUpdates`, `.openLoginItems`:
  through the `LoginItem`, `Permissions` and `Updater` traits, which WP8 implements.
- [x] `settings.recording.setInputDevice`, `.refreshDevices`, `.chooseFolder` (the
  shell's chooser), `.revealFolder`, `.setRetention` (Forever keeps every recording on
  disk through `Pipeline::keep_all_recordings`), `.requestPermission`.
- [x] `settings.transcription.setEngine` (rebuilds the pipeline), `.download`
  (every progress report publishes), `.remove`: through `SpeechModels`, which WP4
  implements.
- [x] `settings.summaries.selectPreset`, `.update`, `.save` (saves on change, then
  probes), `.test`, `.confirmCodex`, `.refreshCodexStatus`, `.refreshCodexModels`,
  `.selectCodexModel`, `.stopUsingCodex`: the API key through
  `steno_core::SecretStore`, the probe and the Codex sign-in through `LlmService`,
  which WP7 implements.
- [x] `settings.export.setEnabled`, `.chooseVault`, `.update`, `.save`: validated
  through `ExportValidator`, which WP7's Obsidian destination implements.
- [x] `settings.iphone.beginPairing`, `.cancelPairing`, `.revoke`: through `Handover`,
  which WP7 implements; the two-second pairing poll is the shell's timer calling
  `Host::refresh_pairing`.
- [x] `onboarding.request` (the page sees `isRequesting` while the prompt is up),
  `.skip`, `.refresh`, `.advance`, `.back`, `.saveSummaries`,
  `.confirmSummariesWithCodex`, `.chooseVault`, `.saveVault`, `.skipSetup`, `.finish`.
- [x] `updates.check`, `system.openURL` (`https:` and `mailto:` only),
  `system.openSystemSettings`, `window.open` (the request rides on `app`),
  `window.close` (onboarding only), `ui.confirmDestructive`.

### Beyond the bridge

- [ ] Menu bar item: the processing queue, the five recent meetings, record and stop,
  the login item toggle, check for updates (`MenuBarViewModel`); the shell's tray, WP8.
- [ ] Floating panels: the recording bubble and the detection prompt with its
  60-second countdown; the shell, WP8.
- [x] Deep links: a requested meeting or Settings section rides on the next `app`
  snapshot and is consumed by that publish (`Host::request_meeting`,
  `Host::open_settings`).
- [ ] Auto-stop after a call ends (the 90-second grace, "Keep recording", the end
  reasons): the recorder's policy, WP5; the snapshot side is covered.
- [ ] Meeting detection (`DetectionController`: one prompt at a time, suppressed while
  recording or when the setting is off): WP5.
- [ ] Retention sweep at launch and after `retentionApplied`, interrupted recordings
  marked failed at launch, unfinished processing resumed at launch: WP6b.
- [ ] Pending speaker reviews (`speakersNeedReview`): not on the bridge; WP6b.
- [ ] Updates: Sparkle today, the Tauri updater at cutover; the `Updater` trait is the
  seam, WP8.
- [x] Login item: registered on the first launch when the setting says so
  (`Host::register_login_item_on_first_launch`), toggled from General, the pane opened;
  the `LoginItem` trait, WP8 implements.
- [ ] Calendar: the event that names a recording and its attendees, looked up at
  recording start: the recorder, WP5.
- [x] Phone pairing: the QR code, a phone's arrival closing the code, a code running
  out, revoke, the listener stopping when no phone is left; the `Handover` trait, WP7
  implements.
- [x] Onboarding opener rule (`Host::should_open_onboarding`): a missing required
  permission, or the flag unset; an install already configured writes the flag and
  stays closed.
- Known differences, settled in WP6a. Fixture values the view models never compute
  are listed, with the host's value and the reason, in
  `crates/steno-host/tests/parity.rs`.
  - The search runs when `meetings.setQuery` arrives (Swift debounced 200 ms; the
    page debounces typing).
  - Dates are worded in English with a 24-hour clock in the zone the shell passes
    (Swift used the locale).
  - One host answers all three windows and routes the four Summaries form commands by
    the calling window (`Host::for_window`), where Swift had one host per window.
  - The Settings sections reload when the stored settings change under them (a store
    change, an onboarding save), where Swift loaded them once per window open.
  - The `recording` throttle is flushed by a thread the host owns, where Swift's
    publisher armed a task.
  - `settings.transcription.download` replies at once and runs on a host thread that
    publishes its progress; a remove while it runs detaches it (its late progress is
    not shown, and the asset shows what the model store reports when it ends), where
    Swift's task kept reporting. Download again while that thread runs reattaches to
    it, as Swift showed the running task's progress.
  - A retried re-export after a refusal happens on the next store change (Swift
    retried on the next `.ready` tick); a pending re-export when the detail goes away
    is attempted once (Swift retried after three seconds in a detached task).

### Store

- `StenoJSON` date output truncates to the millisecond; Rust rounds like GRDB; fix the
  Swift formatter before cutover.
- `MeetingStore.init` should check the migrator's `hasBeenSuperseded` and refuse a
  database with an identifier it does not know, as the Rust store does
  (`StoreError::UnknownMigration`); today GRDB ignores unknown identifiers and the
  Swift app would run on a newer schema without noticing.
- `RecordingIntake.admit` should have the copied master (fsynced with its folder), the
  receipt and the meeting-row commits on disk before the Mac answers `complete`,
  because the phone then deletes its copy; today the copy is not fsynced and under
  `synchronous = NORMAL` a power loss can roll the commits back. Those commits need
  `FULL` (and `fullfsync` on macOS for the drive cache), here and in the Rust port of
  the intake.
- `HandoverEngine.swift:145` refreshes `lastSeenAt` with `store.save(seen, tokenHash:)`,
  an upsert of the device the gate read before a yield, so a revoke that lands in
  between resurrects the device and its token hash. The Rust engine runs an `UPDATE`
  of the row that still holds the token (`Store::touch_paired_device`); move the Swift
  side to the same `UPDATE` before cutover.

### Adapters

- `ManagedBlock.merge` should manage only the lines that carry a `%%steno:` marker
  (sorted where the first of them stood) and copy every other non-blank line of the
  block through in place, as the Rust port does; today a page whose end marker the
  user deleted is re-sorted and blank-stripped from the dangling start marker to the
  far end marker on the next delivery. Rust also treats a line holding only `\r`
  inside the block as blank; Swift's `.whitespaces` does not.
- `AtomicFileWriter.write` should `fsync` the target's directory after the rename, as
  the Rust writer does on Unix, so the new directory entry is durable along with the
  bytes.
- `AtomicFileWriter.temporaryURL` should put the eight hex digits before the name and
  cut the name so the whole temp name fits in 255 bytes, as the Rust writer does;
  today a target name within 20 bytes of the limit fails with "file name too long"
  before the first byte.
- `ObsidianFolderDestination.checkVault` should reject a `.` component in the people
  folder as well (`./People`), as the Rust `check_vault` does by requiring the
  ledger's plain-relative rule; today the receipt then carries `./People/Anna.md`,
  the ledger's inside-the-root check refuses it, and every later delivery runs as a
  first one (folder pin lost, stale lines never removed). This must land before the
  Mac cutover (WP9): a user who typed `./People` or `.` in the Swift Settings would
  otherwise get `PeopleFolderInvalid` on every Rust delivery.
- `ObsidianFolderDestination.checkVault`'s message should say a plain relative path
  (no `.`, `..` or empty components) when it adopts the rule; the Rust message is
  Swift's verbatim and follows.
- The person-page writer should write file names NFC-normalised, as the Rust writer
  does; Foundation writes `Anna Müller.md` in NFD on APFS, which maps both to one
  file, but a vault synced to a normalisation-sensitive filesystem gets two files.

### Audio

What the audio crate (WP5a) does differently from `StenoAudio`, each a
parity item until a plan says otherwise:

- **Mixdown is 16 kHz mono Int16 WAV, not AAC.** There is no AAC encoder in
  pure Rust; `SymphoniaAudioCodec::mixdown_format()` says `Wav16kInt16` so
  the persist stage names `audio.wav` correctly. Options at cutover: ship a
  small AAC encoder (`fdk-aac` is non-free; `ffmpeg` is too large), accept
  WAV for the optional export, or encode through the platform (AudioToolbox
  on the Mac, Media Foundation on Windows) behind a `cfg`.
- **Whole-file decode.** The decoder reads a lane to one `Vec<f32>` at the
  source rate before resampling; the AVFoundation codec converted in 32 768
  frame chunks. A two-hour 48 kHz lane is 1.4 GB transiently. Chunk the
  symphonia path before the Linux release.
- **Resampling.** 48 kHz masters go through the writer's exact 3:1 FIR
  with its group delay dropped, so the decode is zero-phase on the master's
  time; other rates (the phone's 44.1 kHz) through a 64-tap, 128-phase
  windowed sinc. The Swift codec used `AVAudioConverter` at maximum
  quality; the two are not bit-identical. The 3:1 FIR is flat to 7 kHz;
  the sinc, measured in `crates/steno-audio/tests/codec.rs` from 44.1 kHz,
  is within 0.3 dB to 6 kHz and -1.3 dB at 6.5 kHz, with 12 kHz aliasing
  below -50 dB. Accept at cutover or lengthen the sinc; a plan decides.
- **The sidecar lags the master decode by one group delay.** The live
  16 kHz sidecar is the same FIR run causally, so its onset sits 32 samples
  (2 ms) after the master decode's; `decode` prefers the sidecar, so a
  transcript's timestamps shift by 2 ms depending on which file was read.
  Swift had the same relationship (causal sidecar writer, delay-compensated
  `AVAudioConverter`); `tests/codec.rs` pins both onsets. Either compensate
  the sidecar at cutover or accept 2 ms.
- **AAC priming is not trimmed.** AVFoundation dropped the encoder's
  priming samples; symphonia 0.5 trims them for MP3 (the LAME tag, with
  `enable_gapless`, measured: onset at sample 2 of an ffmpeg encode) but
  parses and ignores the MP4 edit list and does not read `iTunSMPB`, so an
  AAC lane from the phone starts 1 024 samples at 44.1 kHz (23 ms) late
  (measured on `Tests/Fixtures/audio/tone-440-44k1-500ms.m4a`; iOS
  encoders prime 2 112). Fix at cutover: read the `elst` media time from
  the container and drop it before resampling, or accept 23 to 48 ms.
- **AAC-LC only.** AVFoundation also decoded HE-AAC; symphonia decodes
  AAC-LC alone. The iOS recorder writes AAC-LC, so nothing is lost today; a
  plan adds HE-AAC if an import needs it.
- **A stop that waited can lose its turn.** Swift's actor runs a `stop()`
  queued behind a writer failure's or a device loss's finalise right after
  it. The Rust `stop()` waits on a condition variable, and a `start()` can
  take the lock first; the stop then answers `InvalidState` and leaves the
  new recording running, and its caller does not get the failed recording.
- **`steno dev` tooling** (`capture-spike`, `aec-bench`, `audio-devices`)
  is not ported; it arrives with the CLI in WP6.
- **Call mode waits for an output client.** Swift and Rust both clock the
  tap aggregate from the system output. Without the capture permission,
  the IOProc runs only once another client opens the output
  (`.plans/spikes/2026-10-01-spike-rust-capture.md`; `tests/live.rs`
  skips). Whether a GUI session also loses its first seconds is
  unchecked. Check on the Swift app before cutover; a plan decides any
  remedy.

Six Swift defects the port does not share; fix them in Swift if it ships
another release, otherwise the cutover closes them:

- `CaptureSession.finish()` should read the sink's ring overrun counts
  before `sink.clear()`, as the Rust `finish()` does; today `clear()` zeroes
  them first (`CaptureSession.swift`, the `clear()` before the
  `droppedSamples` read), so `droppedFrames` never holds a ring overrun.
- `CaptureSession` should count the silence a stop cut short in
  `gapSeconds`, as the Rust session does; today a stop while the gap waits
  for relay room leaves that silence in the master and reports 0.
- `CaptureSession` should fail the stop with `writerFailed` when the master
  is gone from disk at finalisation (the meeting folder deleted while
  recording), as the Rust session does; an unlinked file still writes and
  closes without an error.
- `IOProcRunner.deliver` should take the callback's frame count from the
  first source whose buffer carries frames, as the Rust `deliver` does;
  today an unusable first buffer drops the whole callback, the other lanes
  included.
- `CaptureSession` should end `.failed(.writerFailed)` with the recording
  when a write fails while `stop()` drains the relay, as the Rust session
  does; today the `writerFailed` task runs after `stop()` and is ignored,
  so the state reads `.idle` over a master short of what was delivered.
- `CaptureSession.finish()` should count the whole frames left in the
  rings as dropped, as the Rust session does; today a stop between a
  rebuild's successful restart and `resume` clears the new backend's
  audio and reports nothing.

### Handover

- Network: Rust refuses tunnels as Swift does: point-to-point interfaces on Linux and
  macOS; on Windows every adapter but hardware Ethernet and Wi-Fi that is up
  (`advertise::windows_keeps`), which leaves out Wintun, TAP and Hyper-V adapters; and
  `100.64.0.0/10` everywhere. It serves bridges on Linux and macOS, which Swift classes
  `.other`, and does not re-publish after a network change. WP8 decides: restart on
  network change, or re-register.
- Service name: Swift's `HandoverConfiguration.defaultServiceName()` uses
  `Host.current().localizedName` (the computer name in System Settings). The Rust
  default reads `HOSTNAME` or `/etc/hostname` and falls back to `Steno`; the shell
  passes the OS computer name on the Mac (WP9) and on Windows (WP10).

### Bridge

What the bridge crate (WP1) asks of the Swift side before WP6 fills the list:

- Fixtures: one `<topic>.full` fixture per snapshot topic with every optional field
  set, written by `BridgeSamples` next to the existing ones, so the Rust round-trip
  test pins the optional keys that `crates/steno-bridge/tests/optional_fields.rs`
  pins by hand today.
- Fixtures: a task with `priority: low` in `meeting.detail`, so the round-trip pins
  every `TaskPriority` case; `contract_ts_nested_enums_match` in
  `crates/steno-bridge/tests/fixtures.rs` pins it by hand today.

### LLM

- `OutputLanguage.promptName` falls back to Foundation's `en_US` locale names for a
  tag outside the 25-entry table; Rust has no locale data and writes the tag itself.
  Both agree on every tag the fixtures and goldens use; a meeting tagged with a rarer
  language gets "sw" instead of "Swahili" in the prompt on the Rust side until the
  table grows.

Rust fixes these Swift behaviours; each is ported to Swift or accepted before cutover:

- `OpenAICompatibleClient.complete` and `CodexResponsesClient.complete` should step
  the mode down from the mode the rejected request went out under, as the Rust
  clients do; today they step from the mode the client holds when the 400 arrives,
  so two concurrent rejections go from `.jsonSchema` straight to `.promptOnly` and
  announce two downgrades.
- `OpenAICompatibleClient.complete` should resend a request that still carried a
  parameter a concurrent request already got rejected, as the Rust client does;
  today the second rejection for `max_tokens` or `temperature` finds the parameter
  in `rejectedParameters` and fails the completion with HTTP 400.
- `CodexCredentialStore.refresh` should remember a refresh token the endpoint
  refused for good and give every later caller that refusal while the file still
  holds the token, as the Rust store does; today only the callers waiting on the
  same refresh get it, and the next caller posts the dead token again.
- `CodexCredentialStore.refreshOnce` should keep the file as it is when the
  re-read shows `NotSignedIn`, `ApiKeyLogin` or a refresh token other than the one
  posted, as the Rust store does; today it writes the new tokens over it and undoes
  a sign-out, a switch to an API key or a new login made during the refresh.
- `CodexCredentialStore.refreshRereadingOnReuse` should use the re-read file's
  credentials when their access token is already fit to send, as the Rust store
  does; today it refreshes them again and spends the refresh token the CLI just
  rotated in.
- `CodexResponsesClient.complete` should count the one refresh a completion gets
  after a 401 only once a refresh went through, as the Rust client does; today a
  refresh that failed (the token endpoint hiccuped) uses it up, and the retry's
  401 ends the completion.
- `OpenAICompatibleClient.classify`, `CodexResponsesClient.classify`, the
  undecodable-body errors of both clients and `CodexCredentialStore.refreshOnce`
  should redact the whole of a body that is not the model's answer and only then
  cut it on a character boundary, to 4 096, 500 or 300 characters, as the Rust
  clients do (the answer text is not redacted, in either); today `bodyText` cuts
  it to 4 096 bytes, the fallbacks to 500 characters and the refresh to 300 bytes
  before anything is redacted, so a secret straddling a cut leaves its prefix in
  the error.
- `CodexCredentialStore.refreshOnce` should redact the account id too, and decide
  permanent and reused on the code as sent, lowercased, redacting a copy for the
  detail before and after lowercasing it, as the Rust store does; today it
  redacts the two tokens only, and `RefreshError.code` lowercases the code before
  it is redacted, so an echoed account id stays in the detail and a token echoed
  in the code survives case-folded.
- `LLMTransport.redact` should replace each secret of eight bytes or more and its
  JSON-escaped form (as `serde_json` writes it inside a string, with `/` as is and
  as `\/`), longest first, and skip a secret shorter than eight bytes, as the
  Rust `redact` does; today a placeholder key such as `x` or `ollama` garbles
  every error message it occurs in, a secret that contains another is left in
  pieces, and a key with a quote, a backslash or a slash survives in a body that
  escapes it.
- `CodexCredentialStore.write` should print a failed rename's error number after
  its `strerror` text (`Is a directory (os error 21)`) and name the temporary file
  it could not remove, as the Rust store does; today it prints the `strerror`
  text only and ignores a failed remove, so the two details differ and a leftover
  `.auth.json.steno-<uuid>`, which holds the only live tokens, goes unmentioned.

## Progress

One row per package. WP1 to WP3 were a chain; every package after them is one
PR off `main`.

| Package | Branch | PR | State |
|---------|--------|----|-------|
| WP1 workspace and bridge | `feat/rust-bridge` | #153 | merged |
| WP2 store | `feat/rust-store` | #155 | merged |
| WP3 Tauri shell on fixtures | `feat/rust-desktop` | #156 | merged |
| Core protocols and fakes | `feat/rust-protocols` | #162 | merged |
| Bridge on core | `refactor/rust-bridge-on-core` | #161 | merged |
| WP4b CoreML speech backend | `feat/rust-speech-coreml` | #163 | merged |
| WP7a LLM (`steno-llm`) | `feat/rust-llm` | #167 | merged |
| WP7b adapters | `feat/rust-adapters` | #165 | merged |
| WP6a host | `feat/rust-host` | #170 | merged |
| WP5a audio (`steno-audio`) | `feat/rust-audio` | #166 | merged |
| WP4d diarization (`steno-diarize`) | `feat/rust-diarize` | #164 | open |
| WP7 handover | `feat/rust-handover` | #169 | open |

WP4b is `crates/steno-speech-coreml`: `objc2-core-ml` behind one safe module,
the four backend calls, the FluidAudio 0.17.4 heuristics ported
(silence-aligned starts, contiguous-match merge, seam-word collapse, seam-gap
repair, suppressed-token gate), four parallel windows, `SpeechEngine`
implemented, parity harness `steno-coreml-parity`. Inverse text normalisation
is not applied: Steno's Swift path (`ParakeetEngine` to
`AsrManager.transcribe`) never calls FluidAudio's `TextNormalizer`, so the
baseline carries none. Parity numbers: see the PR.

WP5a is `crates/steno-audio`: the rings, Speex AEC over vendored SpeexDSP,
the writer, the session with its device-change rebuild, the synthetic
backend, the macOS live backend, the meeting detector and the symphonia
decoder; PipeWire (WP5b) and WASAPI (WP10) are stubs. The zero-allocation
proof is `crates/steno-audio/tests/realtime.rs`; the ERLE table is
identical to Swift's `aec-bench --synthetic`; the ring tests run under
ThreadSanitizer in CI's `tsan` job; the live Core Audio tests sit behind
`--ignored` in `tests/live.rs`. Parity items: the Audio list above.

What WP7 leaves for the next package: `crates/steno-handover` is a rustls (ring)
listener, TLS 1.3 only, hyper 1 HTTP/1.1, with the pinned verifier (`pinning`), the
rcgen identity in the `SecretStore` as one PEM bundle, pairing, the seven routes, the
inbox and the mdns-sd advertiser; `tests/wire_contract.rs` reads `wire.ts`. The store
gains the `paired_device*` and `handover_receipt` queries. Core's `RecordingIntake`
(copy into the audio folder, enqueue) waits for WP6: no `RecordingLayout` and no
file-URL to path helper in Rust core yet.
