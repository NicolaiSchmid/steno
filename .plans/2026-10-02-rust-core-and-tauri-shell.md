# Rust core and Tauri shell: Steno on macOS, Linux and Windows

Status: the port is merged. Every work package from WP1 to WP10b except WP9b is on
`main`; the port's last pull request, #187, merged on 2026-10-05, and the follow-ups of
`.plans/2026-10-07-stable-promotion.md` since. Left: the Mac cutover (WP9b,
`.plans/2026-10-04-mac-cutover.md`, ordered and gated by
`.plans/2026-10-07-stable-promotion.md`; its code is merged, the rehearsal, the
handoff and the Swift removal are not), the first Linux release and the first Windows
release (the hardware checks, gate G4 for DirectML, unsigned Windows installers), and
the speech settings in the Settings window. "Open after the port" lists each open item
and its owner. On 2026-10-18 `.plans/2026-10-18-simplify.md` cut this file to what is
still open or binding; the as-built notes, the parity list item by item and the
progress table are in git history (this file at commit `328e3bbb9`).

Started 2026-10-02 on branch `refactor/rust-workspace`. Amends
`.plans/2026-09-24-initial-scope.md` (removes "Windows, Linux" from the v1 non-goals for
the next major version and replaces the "Language / UI", "Core" and "Apps" rows of the
platform table) and the web UI decision of 2026-09-29 (one React and Tailwind app in a
`WKWebView`, "no Electron, Tauri, Node at runtime"), which held for the Swift host; the
Tauri shell replaces that host at cutover. The evidence was the cross-platform spikes
of 2026-10-01: capture in Rust and CoreML called from Rust both work, and our own fp32
ONNX export of Parakeet TDT v3 matches CoreML's accuracy (FLEURS German 5.3 % against
5.5 % WER) at 18 times real time on the CPU. The speech-stack decisions and gates this
plan executes are stated where they apply: gate G1 (ONNX accuracy on FLEURS, met on
the CPU), gate G3 (the diarization rebuild, WP4d), gate G4 (DirectML at least three
times the CPU's speed on an integrated GPU, open), decision 4 (DirectML behind a probe
with the CPU as the fallback), decision 5 (no GPU driver in the app's process; the
diarizer stays on the CPU) and decision 6 (Steno's own diarization).

## Goal

One product on three platforms from one codebase: a Rust core, a Tauri 2 shell, and
the existing React web UI unchanged except for its transport. The Mac keeps CoreML
speech on the Neural Engine; Linux and Windows run our fp32 ONNX export of the same
model. The Swift app keeps shipping until the Rust app reaches parity on the Mac and
reads the same database, so the cutover is a download, not a migration.

Not in this plan: CUDA on Linux and a whisper.cpp Vulkan engine (the rest of the
speech stack's GPU work), the iOS recorder (unchanged), and any new product feature.
DirectML on Windows is WP10b; its gate G4 stays open until a Windows machine with a GPU
measures it. Feature work on the Swift app after this plan started is a parity item
for the Rust side.

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
   `.sql` files until cutover; the parity test proves them equal. The `setting` table
   is shared too: the Rust store writes only the keys `Settings` knows, deletes only
   those of them that are now `None`, and keeps the rows it does not know and the
   Obsidian value's fields it does not know (for the same vault only: another vault
   starts without them). Swift's `SettingsStore` still deletes and rewrites every row
   on each save. Both stores fail to load a stored enum value they do not know. So
   until the Swift removal (S9 of `.plans/2026-10-07-stable-promotion.md`), no new
   setting key or stored enum value lands unless its PR says what the rolled-back Swift
   app or an older Rust build does with it.
3. **Audio never leaves the device.** Only these code paths use the network, and a
   new one needs a plan first:
   - the LLM client (`steno-llm`), text only: the prompts and the transcript to the
     summaries endpoint the user set up, its model list, and the ChatGPT sign-in's
     token refresh;
   - a `Destination` (`steno-adapters`), which sends only text over a network; today's
     one destination, the Obsidian folder, writes to a local folder, including the
     audio mixdown when the user turns that on, and opens no connection;
   - the model downloads, which send nothing but the request: `steno-speech`'s
     `ModelStore` (`crates/steno-speech/src/model_store.rs`) fetches the fp32 Parakeet
     export from Hugging Face at a pinned commit and Silero VAD from a GitHub release
     asset, `steno-diarize`'s two models (`crates/steno-diarize/src/models.rs`)
     from Hugging Face at a pinned commit and a GitHub release asset, and on the Mac the
     `CoreML` Parakeet from the repository FluidAudio reads
     (`FluidInference/parakeet-tdt-0.6b-v3-coreml` on Hugging Face) at a pinned commit,
     or all of them from the mirror the speech settings name; in the app only Settings
     and onboarding start a download, never a pipeline run (the app's engines are built
     in `SpeechEngines::new` behind gates, with the sidecar's `SidecarConfig::install`
     and the diarizer both on `steno_speech::Install::Never`, which steno-diarize
     re-exports), while the `steno` command's
     engines, which it builds for itself in `crates/steno-cli/src/wiring.rs`, may
     download on first use for a command a user runs;
   - the Tauri updater, which fetches `latest.json` and the signed bundle from the
     repository's GitHub releases and sends nothing (the `desktop-stable` endpoint in
     `apps/desktop/src-tauri/tauri.conf.json`, the `desktop-beta` one in
     `apps/desktop/src-tauri/src/updater.rs`);
   - the phone handover server (`steno-handover`), which advertises itself over
     Bonjour, serves connections only on the computer's LAN addresses and loopback,
     speaks TLS 1.3 with the self-signed certificate the phone pins, and receives
     pairing requests and the paired phone's recordings; it opens no outbound
     connection.

   The speech sidecar gets its samples on stdin and answers on stdout, never through a
   socket, and opens no connection (WP4c); the diarizer's lanes reach it the same way.
   On Linux the app, not the sidecar, asks the
   systemd user manager over the user bus's Unix socket for a scope of the sidecar's
   own, sending the sidecar's pid, the unit names and the scope's fixed settings, nothing
   else (`crates/steno-speech/src/sidecar/scope.rs`, P6 of
   `.plans/2026-10-07-stable-promotion.md`; simplify step 4 removes these scopes and
   with them that request). ONNX Runtime's telemetry is off in every
   process that opens a session (`init_environment` in
   `crates/steno-speech/src/onnx.rs`, which `steno-diarize` calls too). Until the
   cutover the Swift app keeps its own list in `AGENTS.md`.
4. **One speech pipeline above the tensors.** Chunker, overlap merge and the TDT decode
   loop are shared; the backends are CoreML (`objc2-core-ml`) on the Mac and ONNX
   Runtime (`ort`) elsewhere. ONNX inference runs in a sidecar process; the Mac runs
   Parakeet in process on `CoreML` (the default) and starts the sidecar only to
   diarize. The diarizer's ONNX
   inference runs in the same sidecar on every platform, the Mac included
   (`steno_diarize::SidecarDiarizer`); `steno dev diarize-sweep` and the calibration
   harness keep it in their own process.
5. **No allocation and no lock on the audio thread**, proven by the counting allocator
   in `crates/steno-audio/src/testing/rt.rs` in a test build.
6. **Scope stays the scope.** No new features in the port; a Rust behaviour that
   differs from Swift is a bug unless a plan says otherwise ("Settled differences from
   the Swift app" below lists the ones this plan decided).

## Architecture

```
Cargo.toml                 workspace
crates/
  steno-core/              domain types, protocols (the pluggable boundaries), SQLite store and migrations, settings
  steno-bridge/            the JSON contract (topics, methods, snapshots, params, envelope), fixture tests
  steno-host/              the view models behind the three windows, the bridge host over the store, the shell-side service traits and their fakes
  steno-pipeline/          the post-meeting pipeline and the intakes
  steno-services/          the object graph from the settings: store, pipeline, recorder, handover listener, secrets, updates
  steno-audio/             capture backends (Core Audio taps, PipeWire, WASAPI), ring buffer, AEC, writer, meeting detection
  steno-speech/            VAD, chunker, merge, TDT decoder, ONNX Runtime backend, sidecar client, model store
  steno-speech-coreml/     the Mac's CoreML backend over the shared pipeline
  steno-speech-sidecar/    the child process that runs ONNX speech and the ONNX diarizer
  steno-diarize/           speaker diarization: segmentation and embedding backends, Steno's clustering and refinement
  steno-llm/               OpenAI-compatible and Codex clients, cleanup and summary passes
  steno-adapters/          destinations (the Obsidian folder), export
  steno-handover/          phone handover server (TLS pinned), shared wire contract with mobile/
  steno-macos/             safe wrappers over the Mac framework calls without a safe binding
  steno-cli/               `steno` binary: record, process, export, dev tools
apps/
  desktop/                 Tauri 2 shell: windows, tray, panels, autostart, updater; no logic
  web/                     the React app (moved from apps/macos/web at the cutover, WP9)
  macos/                   the Swift app, unchanged until cutover, then removed
```

Dependency direction is the Swift one: `steno-core` depends on nothing of ours; every
other crate depends on `steno-core`; the shell and the CLI wire them. The shell holds
no logic: view models and the bridge host live in `steno-host` so the CLI, tests and
the shell share them. The host sits above the core and the bridge in its own crate
(WP6a). Everything the host reaches through the system (permissions, login item,
updater, the recorder, the pipeline, speech models, the LLM probe, vault validation,
the handover listener, devices, files, the file manager) is a trait in
`steno_host::services` with a fake in `steno_host::fakes`, so the whole host runs
without a shell on a temporary database. The shell's `fixture-host` feature answers
the bridge from the recorded fixtures instead, opt-in, for UI work without a database.

Platform backends behind traits, two implementations before generalising: `Capture`
(Core Audio, PipeWire, WASAPI, synthetic), `SpeechBackend` (CoreML, ONNX Runtime, fake),
`SecretStore` (the Keychain through `keyring`; the Windows credential store and the
Secret Service through Steno's own clients, which simplify step 5 replaces with
`keyring`; a 0600 file where no keyring answers), `Updater` (Tauri updater on every
platform; Sparkle retires at cutover).

## Transition

Parallel build. The Swift app ships from `main` throughout. The Rust app is usable on
Linux first (no Swift app competes there), then on the Mac once the parity list is
empty, then on Windows once its capture backend passes the capture tests. Cutover on
the Mac is a release that ships the Tauri app as an ordinary Sparkle update, reading the
same database and settings; `.plans/2026-10-07-stable-promotion.md` gives it the new
identifier `com.nicolaischmid.steno.desktop` (its D5).

## Repository setup for three platforms

- Cargo workspace at the root, `rust-toolchain.toml` pinned to stable, `rustfmt`,
  `clippy -D warnings` and a `cargo check` on `rust-version` in CI. Shared dependency
  versions live in the root `[workspace.dependencies]`, crates inherit them.
  `cargo deny` (`deny.toml`) arrived in WP9 with the first signed release: a
  permissive allow list, MPL-2.0 per crate for the Tauri, `directories` and
  `symphonia` trees, advisories (one unmaintained build-time macro of the GTK 3
  bindings ignored with its reason), sources from crates.io only. CC-BY attribution
  for Parakeet is a runtime notice, not a crate licence.
- `.github/workflows/rust-ci.yml`: `ubuntu-latest`, `windows-latest` and the macOS
  runner (`MACOS_RUNS_ON`, same variable as Swift CI) build and test the workspace;
  Apple-only crates compile on every OS with their backends behind `cfg`.
  `.github/workflows/desktop-release.yml` builds, signs and publishes the bundles of
  the three platforms (WP9a).
- Linux build dependencies for Tauri (webkit2gtk, libayatana-appindicator, PipeWire
  headers) in one script, `scripts/setup-linux.sh`, used by CI and by the worktree
  bootstrap.
- `AGENTS.md` workspace table gains the Rust rows; Swift rows stay until cutover.
- Models are never committed (`.gitignore` already covers `*.onnx`, `*.mlmodelc`); the
  speech crate downloads them from a manifest with checksums.

## Work packages

Every package but WP9b is merged; a stub says what each settled that still binds. The
pull request numbers lead to the as-built detail.

- **WP1 workspace and bridge:** done (#153). `steno-bridge` decodes every fixture and
  re-encodes it byte-identically; a Swift fixture without a Rust type fails a test.
- **WP2 store:** done (#155; the protocols and fakes #162, the bridge on the core
  #161). Migrations are SQL files, proven equal to the Swift schema by the parity test
  against `crates/steno-core/tests/fixtures/schema.swift.sql`.
- **WP3 Tauri shell on fixtures:** done (#156). The `fixture-host` feature stays,
  opt-in, for UI work without a database.
- **WP4 speech:** done (WP4a #171, WP4b #163, the shared decoder #182, the CoreML
  backend on the shared pipeline #263, A2 of the stable plan). Its integration notes
  are below.
- **WP4a speech pipeline and ONNX backend:** done (#171). VAD, chunker, merge and the
  TDT decoder are shared by both backends.
- **WP4b CoreML backend:** done (#163). `crates/steno-speech-coreml`: `objc2-core-ml`
  behind one safe module, `SpeechEngine` implemented, the parity harness
  `steno-coreml-parity` and the ignored test `crates/steno-speech-coreml/tests/parity.rs`
  against the Swift transcripts (it needs the models and the calibration corpus on
  Forge and is run by hand, never in CI). No inverse text normalisation: the Swift
  path never applied FluidAudio's.
- **WP4c sidecar and models:** done (#177, #187, the hosted export #189, the services
  on the sidecar #183). Below.
- **WP4d diarization:** done (#164). `steno-diarize` with Steno's own clustering,
  timeline, mapping and refinement (gate G3); its ONNX inference runs in the speech
  sidecar on every platform, the Mac included (A3 of the stable plan).
- **WP5 audio (WP5a):** done (#166). `steno-audio`: the rings, Speex AEC, the writer,
  the session with its device-change rebuild, the synthetic and Core Audio backends,
  the meeting detector, the decoder. The mixdown is 16 kHz mono Int16 WAV, not AAC. The
  zero-allocation proof is `crates/steno-audio/tests/realtime.rs`.
- **WP5b PipeWire capture:** done (#176). One capture stream that Steno links itself to
  the microphone and the default sink's monitor; its live tests,
  `crates/steno-audio/tests/pipewire.rs`, run in CI against a private headless daemon
  (`scripts/pipewire-headless.sh`). The open Linux points are under "Linux capture".
- **WP6 pipeline and host:** done in two PRs. **WP6a host** (#170): `steno-host`, the
  service traits and fakes, the fixture parity suite (`crates/steno-host/tests/parity.rs`,
  which lists the fixture values the view models never compute). **WP6b pipeline**
  (#173, the exits #185): `steno-pipeline`, `steno-cli`, `steno-services`, and the shell
  on the real host. Every exit runs `App::shutdown` first, which stops and saves a
  recording in progress.
- **WP7 LLM, adapters, handover:** done. WP7a LLM (#167), WP7b adapters (#165), WP7c
  handover (#169: rustls, TLS 1.3 only, the pinned verifier, the shared `wire.ts`
  contract test).
- **WP8 shell completion:** done (#172). Tray, floating panels, autostart, updater,
  onboarding permissions per OS, deep links, single instance, dialogs, the installer
  bundles.
- **WP9 Mac cutover and signed releases.** **WP9a:** done (#184): the speech sidecar in
  every bundle as a Tauri `externalBin` (declared in `tauri.release.conf.json`, staged
  by `apps/desktop/scripts/stage-sidecar.sh`, checked in its installed layout by
  `apps/desktop/scripts/check-bundle.sh`), `cargo deny`, Developer ID signing and
  notarisation, `SHA256SUMS` with OpenPGP signatures, unsigned Windows installers, the
  `desktop-v*` release and the `desktop-beta` and `desktop-stable` updater lanes, each
  only moving forward. **WP9b, planned:** the cutover, `.plans/2026-10-04-mac-cutover.md`:
  the blocking list of `.plans/2026-10-07-stable-promotion.md` (D3) closed, the
  identifier `com.nicolaischmid.steno.desktop` (D5), the Sparkle handoff, the Swift app
  removed, the web app moved to `apps/web`, the Swift rows removed from `AGENTS.md`.
- **WP10 Windows.** **WP10a:** done (#175, follow-ups #186): WASAPI capture, never run
  on hardware. **WP10b:** merged (#188), gate G4 open; below. The installer is WP9a's.

### WP4 integration notes

Settled by A2 of `.plans/2026-10-07-stable-promotion.md`: both backends run one decode
loop, one merge, one partition and one segmentation. FLEURS German `cat/`, which has
references, is the accuracy gate (Nicolai, 2026-10-12): through CoreML at or below
5.0 % mean WER, a hard limit that `scripts/coreml-parity-forge.sh` checks (the
variants through `scripts/coreml-parity-variants.py`); `scripts/score_fleurs.py` scores
transcripts (`steno dev bakeoff` output among them) against the FLEURS references with
one normaliser. Measured: CoreML 4.87 % (from 5.46 % on the CoreML crate's
own pipeline, which the Swift app's transcripts score too), the ONNX engine 4.88 %. The
test against the Swift transcripts of the calibration corpus tracks drift only: 8.46 %
mean per-file WER under its limit of 10 %, and word starts +4.2 ms from Swift's under
its limit of 15 ms, because the windows are laid out differently.

- Decoder: NeMo's rules (`DecoderConfig::default()`): ten symbols a frame, 40 tokens a
  second of window plus 16, the window-end token emitted (the merge owns the overlap).
- Merge: the LCS over 2 s of overlap on both backends (`ChunkerConfig::default`); no
  seam repairs, as FluidAudio's gained one word in 70 minutes.
- Recovery: the shared gate and retry; FluidAudio's perturbation ladder is not ported.
- Chunking on CoreML: the VAD layout under a 15 s window, no long-pause skip (it cost
  words), the level-following `AdaptiveEnergyVad`, four workers. The gaps the chunks
  leave (the lead-in, the tail, the rest of a pause) are decoded in extra windows when
  they last 2.5 s or more (`steno_speech::chunker::MIN_GAP_SECONDS`), with the recording
  split once into owned spans (`steno_speech::chunker::partition`, A16a). Known limit:
  a gap under 2.5 s is not decoded, as windows that short insert words. Both backends
  time words from each chunk's exact start (`Token::phase`, A13).
- The settings that still differ, CoreML then ONNX: the clamp (15 s, 60 s), the
  long-pause skip (none, 3 s), the extra windows (every gap; the tail only, with the
  first chunk from the recording's start, A16b), the detector (`AdaptiveEnergyVad`,
  Silero) and the workers (four, one).

### WP4c sidecar and models

`crates/steno-speech-sidecar` hosts the ONNX `Transcriber` and the diarizer;
`SidecarSpeechEngine` in `crates/steno-speech/src/sidecar/` implements `SpeechEngine`
over it.

- Protocol: over the child's stdin and stdout only, never a socket or a file. A frame is
  a little-endian `u32` header length, the JSON header in the bridge convention, then
  the payload the header declares (a request's samples as little-endian `f32`). Headers
  are capped at 64 MiB.
- Limits: the parent installs the models (the child opens no connection), enforces a
  per-request deadline and a memory ceiling (6 GiB) against the child's heartbeat, and
  starts only an absolute program path, never one looked up on `PATH`. A child that
  dies, hangs, overruns or breaks the protocol is killed, the call fails, and the next
  call starts a new child. The child exits when its stdin ends, so a dead app leaves no
  child. `SpeechEngine::release()` frees its working set once no job needs it. Simplify
  step 5 replaces the supervisor with a plain one.
- Platform policy (`crates/steno-speech/src/runtime.rs`): on Linux and Windows the
  sidecar is the only speech engine the app runs; on macOS the in-process CoreML engine
  is the default and the sidecar a fallback behind `SpeechSettings::onnx_sidecar_on_mac`.
- Models: the fp32 export (2.6 GB) is hosted in the public Hugging Face repository
  `nicolaischmid/steno-models` (`STENO_MODELS_REPO`), uploaded with its CC-BY-4.0
  `ATTRIBUTION.md` by `scripts/upload-models.sh` and pinned at commit
  `4a133253481bfd2cb38dc3e77c3f748199562488` (`PARAKEET_V3_FP32_REVISION`); a personal
  account, so moving it later means a new upload and a new pin. Silero VAD and one of
  the diarizer's models come from GitHub release assets. A mirror
  (`SpeechSettings::models_mirror`, `STENO_MODELS_MIRROR`) replaces every host with no
  fallback, so it serves a whole store root. Simplify step 5 reduces the download to a
  `.partial` file, a SHA-256 check and a rename.

### WP10b DirectML

The speech encoder runs on DirectML on Windows when `directmlOnWindows` asks for it;
the decoder, the joiner, Silero and the diarizer stay on the CPU (decision 5). The probe
is the session itself: DirectML in the ONNX Runtime build, a hardware DirectX 12
adapter (WARP left out), the session created, one encoder run on a second of silence.
Any failure opens the encoder on the CPU; a child that crashes, hangs or overruns the
ceiling with DirectML in use switches DirectML off for the rest of the app's run. What
is not proven:

- **Off by default.** Gate G4 is open with no machine, and so is whether DirectML's
  FLEURS transcripts match the CPU's (G1 was measured on the CPU). The default flips
  when a Windows machine with a GPU has measured both; the same machine checks which
  nodes ONNX Runtime leaves on the CPU (its verbose session log) and whether a hung
  DirectML child exits when killed.
- **`DirectML.dll`** is a load-time import of every Windows binary that links ONNX
  Runtime. The bundles install it beside the binaries, as the copy in `System32` is
  DirectML 1.8 only from Windows 11 22H2; the G4 machine needs the DLL beside the
  binary. Neither case has run on Windows.
- **Privacy.** ONNX Runtime's telemetry stays off. With DirectML on, `DirectML.dll` and
  Direct3D 12 may log to Windows' own diagnostic data as for any program that uses
  them; Steno opens nothing for it, and no audio or text is involved. Before the
  default flips, the measurement records which event providers a DirectML session
  uses, and the bundles carry the redistributable's licence notice, which they do not
  yet.
- **Not done:** a device choice (the system's default GPU is used) and the setting in
  the Settings window (the `SpeechSettings` item under "Parity list").

## Risks

- Tauri floating panels are webviews; the recording bubble and the detection prompt
  get re-tuned on the Mac once they run there.
- Two shells and two cores until the cutover: the parity list and the fixture oracle
  keep them honest.

## Parity list

Every user-visible behaviour of the Swift app, ticked when the Rust app matches it on
the Mac. Every line is ticked except the first under "Speech"; the per-item list with
its tests is in git history (commit `328e3bbb9`). Feature PRs on the Swift app until
the cutover add a line here.

### Speech

- [ ] `SpeechSettings` (`onnxSidecarOnMac`, `directmlOnWindows`, `modelsMirror`) are
  Rust-only: Swift has none of them. `steno-services` reads them from `speech.json` in
  the support directory (`steno_services::speech::speech_settings`), not from the
  `setting` table, which the Swift app rewrites whole on every save;
  `STENO_MODELS_MIRROR` overrides the mirror, which serves the diarizer's models too, so
  it holds a whole store root. Nothing writes the file and the bridge contract has no
  field for any of them, so the Settings window shows none:
  `.plans/2026-10-07-speech-settings-ui.md` proposes their place and wording.
- [x] One model store: the diarizer's two models are the `steno_speech::ModelAsset`
  `diarization` (`crates/steno-diarize/src/models.rs`), installed by
  `steno_speech::ModelStore` into `<models directory>/onnx/diarization/`, with the
  store's lock, resume, progress and mirror (`<mirror>/diarization/<file>`). Who may
  download is the caller's `steno_diarize::Install`: the app builds its diarizer on
  `Never`, so a missing model makes the meeting wait for the models instead of ending
  with the one room speaker; `steno process` stays on `Allowed`, since a command run in
  a terminal may download on first use. Rust only, like the next rule.
- [x] No pipeline run downloads a model (S1 of `.plans/2026-10-07-stable-promotion.md`):
  a meeting refused for missing models stays `queued` and resumes once Settings or
  onboarding installed them. Simplify step 5 replaces the gates
  (`crates/steno-services/src/model_gate.rs`) with one "models installed?" check.

### Settled differences from the Swift app

Invariant 6 holds these as decided, not as bugs:

- The web app follows the store through a two-second poll where Swift had GRDB
  observation; the search runs when `meetings.setQuery` arrives (the page debounces);
  dates are worded in English with a 24-hour clock in the zone the shell passes; one
  host answers all three windows (`Host::for_window`).
- A chosen microphone that is missing or does not open no longer ends a recording
  while the default input can be opened: the capture records the default input in its
  place, and Settings keeps the choice, listed as "Microphone not connected".
- An API key the secret store cannot read is never deleted: a save writes the key only
  when the field changed.
- A ready meeting whose export is still `pending` is re-exported at launch; Swift
  re-exported only when asked. A re-export keeps a note the user edited in the vault
  and writes Steno's version beside it (D13 of the stable plan); a rolled-back Swift app
  overwrites it, a loss accepted under D3 and D9.
- A re-run keeps a stored speaker confirmation and the name suggestion for a speaker id
  that comes back and recomputes the voices of the persons involved (decision 5 of the
  speaker calibration, 2026-09-29); a kept confirmation whose new row has no clip keeps
  its earlier clip.
- The mixdown is 16 kHz mono WAV; the resampler's fold above 7 kHz was accepted by
  Nicolai on 2026-10-08 (stable plan D9 and A9). A damaged AAC packet becomes silence
  and a phone recording without an index is salvaged (A14); Swift did neither.
- Schema v5 adds the handover admission ledger, `handoverAdmission` (recording id, byte
  count, SHA-256, meeting id, admitted at; no foreign key), in both apps (#243): an
  announce of bytes already admitted is answered as delivered, also after a revoke or a
  meeting delete, so a phone that lost the `complete` answer does not deliver them
  twice. Every store open backfills it from the `complete` receipts whose meeting
  exists.
- The tray lists pending speaker reviews and counts them on its icon; Swift drew no
  badge. The pairing poll runs whether or not a code is shown. The calendar lookup
  never holds a recording start up.
- "Process again" (P9 of the stable plan), the free-space watch while recording, launch
  recovery of an interrupted recording from its master, and the crash log file are
  Rust only.

## Linux capture

What the PipeWire backend (WP5b) does differently from the Mac's, to settle before the
first Linux release:

- **The system lane is the whole default sink.** The Mac's tap leaves out Steno's own
  process; the PipeWire backend records the default sink's monitor, Steno's output
  included, and only that sink: an app routed to another output is not in the lane.
  No in-app playback runs while recording (`steno_audio::Playback`), so nothing of
  Steno's own is in the lane. Leaving its output out was weighed and not done (#214):
  it would need a second linking policy beside WirePlumber's, one link per playback
  stream into the default sink, and is still blind behind a filter (a loopback, an
  equaliser or an echo-cancel sink mixes before the sink). Once Steno plays audio while
  it records, pausing that playback, or sending it to another device, may be simpler.
- **Missed graph cycles are filled, and real-time scheduling is checked** (A18). A
  data-loop thread the scheduler holds up misses graph cycles; the stream's `pw_time`
  ticks count them, and the gap is written as silence and counted. The client context
  loads `module-rt`, which asks for real-time scheduling of the data-loop thread (under
  `RLIMIT_RTPRIO`, or through RealtimeKit); without it `start` logs once at `warn`. In a
  starved 1-CPU container Steno missed 0.15 to 0.70 s in 10 s without real-time
  scheduling and 0 to 0.09 s with it.
- **A default move can go unreported before PipeWire 1.6.9
  (pipewire#5445).** With WirePlumber 0.5.14 and
  PipeWire 1.6.5, a client binding the `default` metadata holds its
  events back from every client bound before it until WirePlumber answers
  the bind, so a move made meanwhile never reaches Steno's capture (seen
  when the live tests polled the metadata with `pw-metadata`). In
  `src/modules/module-metadata/metadata.c`, `global_bind` pings
  WirePlumber and raises `pending` (lines 189 and 190), and
  `metadata_property` forwards an event only while `pending` is 0 or to a
  binder still waiting for its pong (line 53). `global_unbind` (line 106)
  never calls `remove_pending` (line 122), so after a binder leaves before
  its pong, `default` events stop for every client, later binders
  included once their own pong is answered, until WirePlumber restarts;
  restarting Steno does not help. Steno's own start can leave a bind
  unanswered: if WirePlumber stalls past `START_TIMEOUT` (3 s),
  `Capture::open` drops the connection with the pong outstanding, and
  each retry of the rebuild adds another. The metadata proxy keeps no
  copy to read again. #201 tried a periodic re-read (a fresh bind every
  3 s) and dropped it: its own binds caused the same miss for the other
  clients (pipewire-pulse, the desktop's sound settings) and could stop
  their events for good. Reproduced on the private daemon, in a shell
  under `scripts/pipewire-headless.sh bash`, with `$wp` set to
  WirePlumber's `application.process.id` from `pw-dump`:

  ```sh
  pw-metadata -m -n default &             # bound first; prints every event
  kill -STOP "$wp"                        # no pong from now on
  timeout 1 pw-metadata -n default        # a second bind, gone before its pong
  kill -CONT "$wp"
  pw-metadata -n default 0 steno.after 1  # the monitor never prints it
  pw-metadata -n default                  # a fresh bind lists it
  ```

  With the second bind run in the background instead
  (`pw-metadata -n default &`), so it outlives the pong, the monitor
  prints the set.

  Upstream (checked 2026-10-07, #214): this is
  [pipewire#5445](https://gitlab.freedesktop.org/pipewire/pipewire/-/work_items/5445),
  reported against PipeWire 1.6.8 with WirePlumber 0.5.15 and fixed by
  "metadata: remove pending pong on unbind" (06de0ed2 on `master`,
  cherry-picked as b784720b on `1.6`), which calls `remove_pending`
  first thing in `global_unbind`. PipeWire 1.6.9 (2026-09-17) is the
  first release with it. `metadata.c` is identical in 1.2.7, 1.4.11, the
  head of the `1.4` branch and 1.6.5 to 1.6.8, and has the same code at
  the same lines in 1.0.5, so a distribution on 1.0 to 1.4 keeps the
  miss unless it carries the fix. Both variants of the reproduction
  still hold with PipeWire 1.6.5 and WirePlumber 0.5.14;
  neither was run against 1.6.9. Nothing is left to report for 1.6. A
  backport to 1.4 could still be asked; the text below is ready and not
  filed (Nicolai files it if the Linux release targets a distribution on
  1.4):

  > **metadata: backport "remove pending pong on unbind" (#5445) to 1.4**
  >
  > PipeWire 1.4.11 and the `1.4` branch still have the bug that
  > 06de0ed2 fixed on `master` (b784720b on `1.6`, released in 1.6.9).
  > In `src/modules/module-metadata/metadata.c`, `global_bind` pings the
  > metadata's owner and raises `impl->pending` (lines 189 and 190);
  > `metadata_property` forwards an event only while `pending` is 0 or
  > to a resource still waiting for its pong (line 53); `global_unbind`
  > (line 106) removes the pong listener without calling
  > `remove_pending` (line 122). So a client that unbinds before the
  > owner answers its ping stops property events for every bound client
  > until the owner restarts. Reproduced with PipeWire 1.6.5, whose
  > `metadata.c` is identical to 1.4.11's, and WirePlumber 0.5.14:
  >
  > ```sh
  > wp=$(pgrep -u "$(id -u)" -x wireplumber)
  > pw-metadata -m -n default &
  > kill -STOP "$wp"
  > timeout 1 pw-metadata -n default
  > kill -CONT "$wp"
  > pw-metadata -n default 0 test.after 1
  > pw-metadata -n default
  > ```
  >
  > Expected: the monitor prints `test.after`. Actual: it never does,
  > while a fresh `pw-metadata -n default` lists it. Could 06de0ed2 go
  > into 1.4?
- **Meeting detection names a holder by its binary.** A process holds the microphone
  while one of its `Stream/Input/Audio` nodes is linked from a source and running;
  Steno's own capture, a filter's or loopback's halves and level meters are left out.
  The name is `application.process.binary`, else `application.name`, so a call in a
  browser tab names the browser's binary, not the call service. The pid is not always
  the host's: a PulseAudio client's `pipewire.sec.pid` is the pulse server's, and a
  Flatpak app's `application.process.id` is its pid inside the sandbox, so two
  sandboxed apps can merge into one holder.

### Measuring the latencies on real hardware

No Linux machine with real audio devices has measured the capture's
latencies; the private daemon's null devices report zero. On a Linux
laptop with PipeWire and WirePlumber, run these once with the built-in
speakers and microphone and once with a Bluetooth headset in its headset
(call) profile, in a quiet room, with no other app playing or recording,
the speakers loud enough to hear the clicks (hold the headset's earpiece
against its microphone):

1. Build once. Install Rust with rustup (the repository pins its
   toolchain) and clone the repository. Install the build packages:
   Debian and Ubuntu `scripts/setup-linux.sh`; Arch and Omarchy
   `sudo pacman -S --needed base-devel clang pipewire wireplumber python`;
   Fedora `sudo dnf install gcc gcc-c++ clang-devel pipewire-devel python3`;
   NixOS the nix-shell the header of `scripts/setup-linux.sh` describes.
   Then, in the checkout, `cargo build --release -p steno-cli`. The first
   build downloads ONNX Runtime and takes several minutes.
2. Make the device under test the default: for the headset, pick its
   headset profile in the desktop's sound settings (Configuration or
   Profile: "Headset"), then check `wpctl status`. The default sink and
   source carry a `*`; if the `*` is not on the device under test,
   `wpctl set-default <id>` with the id `wpctl status` shows. The
   capture's system lane and `pw-play` both use the default sink.
3. Note the versions and devices: `pipewire --version`,
   `wireplumber --version`, `wpctl status` and
   `target/release/steno dev audio-devices` (the inputs with their UIDs).
4. Write a click track, one 1 ms click a second for ten seconds:
   `python3 -c 'import wave,struct; w=wave.open("/tmp/click.wav","wb"); w.setnchannels(1); w.setsampwidth(2); w.setframerate(48000); w.writeframes(b"".join(struct.pack("<h", 20000 if i % 48000 < 48 else 0) for i in range(480000)))'`
5. Record a call and play the clicks two seconds into it, so the first
   click reaches both lanes, then read the log without the level lines:
   `RUST_LOG=steno_audio=debug target/release/steno dev capture-spike --lanes call --seconds 8 --out /tmp/steno-latency > /tmp/steno-latency.log 2>&1 & sleep 2; pw-play /tmp/click.wav; wait; grep -v dBFS /tmp/steno-latency.log`
6. Note from the log: the `info` line "the PipeWire capture runs N ms
   after start: input latency A frames, output latency B frames", the
   `debug` line "the first cycle, C frames, came D ms after linking", and
   the `onset mic` and `onset system` lines. The round trip of a click
   through the speakers and the microphone is `onset mic` minus
   `onset system`; the reported latency is (A + B) / 48 000 s. The echo
   canceller needs the reported one below the round trip, with the room's
   share (a few ms) and the Speex tail (200 ms) covering the rest.
   Check the onsets before using them: each is the first sample above
   0.01 anywhere in its lane, so a pop when the device opens, a
   notification or music gives a wrong or negative round trip. `onset
   system` should read about 2.0 s and `onset mic` a little later (a few
   ms on built-in devices, up to a few hundred ms over Bluetooth). An
   `onset mic` of -1 means the microphone did not hear the clicks: louder,
   or closer. Otherwise close other audio apps and run step 5 again.
7. Repeat step 5 three times per device, noting step 6's numbers after
   each run (every run overwrites `/tmp/steno-latency.log`), and note the
   spread.

Add the numbers, with the device names and versions, under this heading
in a pull request, which also deletes the latency item under "First Linux
release" in "Open after the port". They settle whether the lower bounds
are right on ALSA and Bluetooth.

## Open after the port

What the merged packages left open. Each item starts with its owner: **WP9b** (the Mac
cutover, `.plans/2026-10-04-mac-cutover.md`; which items block the stable release, and the
package that closes each, are in `.plans/2026-10-07-stable-promotion.md`), **First Linux
release**, **First Windows release** (WP10's hardware checks and the unsigned installers),
the branch of a follow-up pull request, or **Unowned** (no package or release has it yet).
Then it says what is open, where it lives and which pull requests found it. The pull
request that fixes an item deletes it. Whatever its owner, an item that can lose a
recording, a transcript, a note or a pairing blocks the stable release
(`.plans/2026-10-07-stable-promotion.md`, D3). An item that a row of that plan's tables
names belongs to that row's package (S, A, P or X), whatever its label here.

- **Superseded by simplify step 4.** The Linux session-end and lost-display handling and
  the 20 s stop-timeout drop-ins (`apps/desktop/src-tauri/src/session_end.rs`,
  `apps/desktop/src-tauri/src/display_lost.rs`, #227) were never run under a real GNOME
  or KDE Plasma logout; `.plans/2026-10-18-simplify.md` removes them and accepts that a
  logout can lose a recording, so they are no longer open work here.
- **First Linux release.** Every destroyed webview leaks a file descriptor on Linux
  (issue #160): wry's IPC handler (`attach_ipc_handler` in wry 0.57's
  `src/webkitgtk/mod.rs`) holds the webview it belongs to, a reference cycle
  through the view's user content manager, so the view is never finalised and
  WebKitGTK never frees its memfd. The app works around it on Linux by destroying
  no window while it runs (a closed Settings or onboarding window is kept and loads
  afresh when opened again, #220); the cycle itself is not fixed and not yet
  reported to wry. To do: report it there with the reproduction outside the app,
  and drop the workaround once a fixed wry ships. Where:
  `apps/desktop/src-tauri/src/windows.rs` (`Kept`), `apps/desktop/README.md`.
  Found: #172.
- **First Linux release.** The PipeWire backend's system lane is the whole default sink
  (Steno's own output included; leaving it out was weighed and not done). Where: "Linux
  capture". Found: #166, #176.
- **First Linux release.** Whether Steno's PipeWire data-loop thread gets real-time
  scheduling on each target: RealtimeKit installed and running (NixOS:
  `security.rtkit.enable`), or an `RLIMIT_RTPRIO` for the user. Without it a busy
  computer makes the capture miss cycles, which are filled with silence and counted
  (A18), and `start` logs a `warn` line; check that the line does not appear on
  Omarchy, NixOS and a GNOME distribution (the stalled recording in the stable plan's
  Linux gates). Where: "Missed graph cycles are filled" under "Linux capture". Found:
  A18.
- **First Linux release.** The capture's latencies are unmeasured on real hardware:
  Nicolai runs "Measuring the latencies on real hardware" on a laptop with ALSA and
  with a Bluetooth headset. Found: #176.
- **First Linux release.** A default move can go unreported on PipeWire before 1.6.9
  (pipewire#5445, fixed upstream by 06de0ed2 on `master` and b784720b on `1.6`; seen
  with 1.6.5 and WirePlumber 0.5.14). Check the versions the release's distributions
  ship; for one on 1.4, file the backport request whose text is ready (not filed); on
  1.0 or 1.2 only the distribution's own package can carry the fix. Where: "A default
  move can go unreported" under "Linux capture". Found: #197, #201.
- **First Linux release.** Meeting detection on PipeWire names a holder by its binary
  (`application.process.binary`, else `application.name`), so the prompt says
  "firefox opened the microphone" (`steno_services::detection::fallback_app_name`);
  it needs a display name, from the app's `.desktop` entry or `application.name`. A
  Flatpak app's pid is its pid inside the sandbox, so two sandboxed apps can merge
  into one holder; check with a Flatpak browser on a real desktop, where
  `pipewire.access.portal.app_id` may name the app better. Where: "Meeting detection
  names a holder by its binary" under "Linux capture". Found: #222.
- **First Windows release.** Meeting detection names a Windows holder by its
  executable without `.exe` (`steno_services::detection::fallback_app_name`), so the
  prompt says "Teams opened the microphone" and an auto-stop stores "Teams" as the
  call's app; a display name (the executable's `FileDescription`) would read better.
  Check the names on a real call with the Teams and Zoom desktop apps. Where: S2 of
  `.plans/2026-10-07-stable-promotion.md`. Found: #271.
- **First Windows release.** Gate G4 is open: no Windows machine with a GPU has
  measured DirectML's speed (at least three times the CPU's on an integrated GPU), so
  `directmlOnWindows` stays off by default (`SpeechSettings` in
  `crates/steno-speech/src/runtime.rs`). The same machine checks that DirectML's
  FLEURS transcripts match the CPU's, which nodes ONNX Runtime leaves on the CPU, and
  which event providers a session uses. Before the default flips, the Windows bundles
  must also carry the licence notice of the `DirectML.dll` they ship
  (`apps/desktop/src-tauri/tauri.release.windows.conf.json`). Where: "WP10b DirectML".
  Found: #188.
- **First Windows release.** A sidecar killed at its deadline is then waited for
  without a bound (`kill` in `crates/steno-speech/src/sidecar/client.rs`), so a child
  stuck in a GPU driver call could hold the engine's lock; the G4 machine checks
  whether a hung DirectML child exits when killed. Where: "WP10b DirectML". Found: #188.
- **First Windows release.** A Windows logoff or shutdown reaches the shell as
  `RunEvent::Exit` (tao answers `WM_ENDSESSION` with it) and gives the app about five
  seconds, less than `SHUTDOWN_PATIENCE` (10 s, `crates/steno-services/src/app.rs`),
  and the shell does not call `ShutdownBlockReasonCreate` while a recording runs, so a
  long save at logoff can be cut off; untested on hardware. Whether a logoff ends the
  speech sidecar, a console process, before the pipeline quits (which would mark its
  meeting `failed`) is unverified. Where: `apps/desktop/src-tauri/src/main.rs`. Found:
  #185.
- **First Windows release.** No Windows machine has run the WASAPI backend: the
  `--ignored` tests in `crates/steno-audio/tests/live_windows.rs`, a real call over
  process loopback, a device switch while recording, the slip rate between the
  microphone's and the system audio's clocks (no drift compensation), detection by
  executable name and the device list Settings > Recording shows. CI's
  `windows-latest` runner has no audio endpoint and runs only process loopback, which
  delivers silence. Found: #175, #186.
- **First Windows release.** The `.msi` and NSIS installers are not code-signed (no
  certificate), so SmartScreen asks before the first install. Where:
  `.github/workflows/desktop-release.yml`, WP9a. Found: #184.
- **First Windows release.** WebView2 keeps its own browser keys: on the pages that
  do not bind them (Settings, onboarding) Ctrl+Shift+R reloads the page and Ctrl+F
  opens WebView2's find bar. Tauri 2.12 does not expose wry's switch for them
  (`AreBrowserAcceleratorKeysEnabled`); untested on Windows. Where:
  `apps/desktop/src-tauri/src/windows.rs`, `apps/macos/web/src/lib/platform.tsx`.
  Found: #204.
- **Unowned.** The speech settings (`onnxSidecarOnMac`, `directmlOnWindows`,
  `modelsMirror`) live only in `speech.json`, which nothing writes, and the bridge has
  no field for them; `.plans/2026-10-07-speech-settings-ui.md` proposes where they
  appear and how they read, and the item stays open until that UI ships. Where: the
  `SpeechSettings` item under "Speech" in the parity list. Found: #177, #187.
- **Unowned.** Untested paths with no seam to test them: the system calls behind the
  phone intake's syncs (Rust's `Disk` in `crates/steno-pipeline/src/files.rs`, Swift's
  `RecordingIntake.Syncs.disk`; the tests record which syncs run, not that the disk
  flushed), and in `steno-llm` the cleanup after a failed `auth.json` write and the
  detail that names a temporary file that could not be removed. Where:
  `crates/steno-pipeline/src/files.rs`,
  `Sources/StenoCore/Storage/RecordingIntake.swift`, `crates/steno-llm`. Found: #167,
  #185, #213.
- **Unowned.** One loss ledger per stream, in place of A18's counting: each stream
  compares the frames its clock expected with the frames its thread delivered,
  reconciled once at its stop, and the session merges the lost spans by union on the
  recording's timeline, so nothing is subtracted and the note no longer takes only the
  worst lane. Nicolai's decision makes it the first capture PR after the stable
  release, landing with the capture seam and scripted fake of
  `.plans/2026-10-15-recorder-actor.md` (its PR 1), under a new D9 ruling. Where:
  `crates/steno-audio/src/capture/session.rs`, `crates/steno-audio/src/realtime/`,
  A18 and "What follows" in `.plans/2026-10-07-stable-promotion.md`. Found: #291,
  #297.
- **Unowned.** A Pair that lands during the quit starts a listener after it:
  `App::shutdown` stops the listener, but `ListenerHandover::close` only keeps a
  waiting handover from receiving one, so a `start` waiting on the stop's lock binds
  once the stop returns and runs until the process exits. It loses nothing. With the
  fixed port, an update's relaunch that binds before the old process exits falls back
  to a port the system chooses, which the firewall blocks until the next launch. The
  likely remedy: `ListenerHandover::start` refuses once `close` ran. Where:
  `crates/steno-services/src/handover.rs`, `crates/steno-services/src/app.rs`
  (`shutdown`). Found: #276.
