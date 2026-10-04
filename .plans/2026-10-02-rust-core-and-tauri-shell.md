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
3. **Audio never leaves the device.** Only `Destination` implementations, the LLM
   client and the updater open network connections: the LLM client sends text, and
   the updater fetches the release manifest and the signed bundle from the endpoint
   in `apps/desktop/src-tauri/tauri.conf.json` and sends nothing.
4. **One speech pipeline above the tensors.** Chunker, overlap merge and the TDT decode
   loop are shared; the backends are CoreML (`objc2-core-ml`) on the Mac and ONNX
   Runtime (`ort`) elsewhere. ONNX inference runs in a sidecar process; the Mac stays
   one process while `CoreML` runs Parakeet (the default). The diarizer's ONNX
   inference does not run in the sidecar yet: see the open item under "Pipeline and
   services (WP6b)".
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
  desktop/                 Tauri 2 shell: windows, tray, panels, autostart, updater; bridge host
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
runs on every platform before the pipeline exists; since WP6b the real host is the
default and the feature is opt-in, for UI work without a database.

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
  `cargo deny` (`deny.toml`) arrived in WP9 with the first signed release: a
  permissive allow list, MPL-2.0 per crate for the Tauri, `directories` and
  `symphonia` trees, advisories (one unmaintained build-time macro of the GTK 3
  bindings ignored with its reason), sources from crates.io only. CC-BY attribution
  for Parakeet is a runtime notice, not a crate licence.
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
  Integration notes (WP4a `crates/steno-speech`, #171, against `crates/steno-speech-coreml`
  of #163): the checklist for moving the CoreML pipeline onto the shared one. Invariant 4
  makes the two pipelines one; each item is a place where they differ today. "Measure"
  means: run FLEURS German `cat/` with both choices and keep the better mean.
  The decode loop is one loop: `decode_frames` in `crates/steno-speech/src/decoder.rs`
  over the `TdtModel` trait, which `decode_window` in the same file builds over a
  `SpeechBackend` and `WindowModel` in `crates/steno-speech-coreml/src/backend.rs` over
  the CoreML models. The ONNX pipeline runs it under `DecoderConfig::default()` (NeMo),
  the CoreML crate under `FLUID_AUDIO` in `crates/steno-speech-coreml/src/decoder.rs`,
  whose `decode_window` keeps the short-window exit, the tail flush and the emission
  cutoff. The first three decode-loop items below are a choice between those two
  configurations; the last three are the steps the CoreML `decode_window` keeps. All
  six are settled when the CoreML backend moves onto the shared chunker; until then each
  backend keeps its own, so FLEURS and the Swift parity both hold.
  - Decode loop:
    - [ ] Repeated zero-duration tokens. Here: `crates/steno-speech/src/decoder.rs`
      (`DecoderConfig::max_symbols_per_frame`, NeMo's `max_symbols` 10). There:
      `crates/steno-speech-coreml/src/decoder.rs` (`FLUID_AUDIO`: two symbols a frame,
      the forced advance recorded as the duration, `TokenDuration::Advanced`). Resolve: measure.
    - [ ] Token budget. Here: `crates/steno-speech/src/decoder.rs` (`TokenBudget::PerSecond`,
      40 a second of window plus 16). There: `crates/steno-speech-coreml/src/decoder.rs` (`MAX_TOKENS_PER_CHUNK`,
      150 a window, `TokenBudget::PerWindow`). Resolve: the per-second budget; 150 truncates a 60 s chunk.
    - [ ] Window end. Here: `crates/steno-speech/src/decoder.rs` (`decode_frames`, `WindowEnd::Emit`: a token
      whose duration passes the window end is emitted, as NeMo does). There: `crates/steno-speech-coreml/src/decoder.rs`
      (`FLUID_AUDIO`, `WindowEnd::Drop` drops it; only the last window's flush
      recovers it). Resolve: emit it; FLEURS passes here without the
      flush.
    - [ ] Short window. Here: `crates/steno-speech/src/decoder.rs` (`decode_frames`, one frame decodes).
      There: `crates/steno-speech-coreml/src/decoder.rs` (`decode_window`, empty for `valid <= 1`). Resolve:
      either; FLEURS never makes a one-frame window.
    - [ ] Tail flush. Here: none, the merge owns the overlap. There: `crates/steno-speech-coreml/src/decoder.rs`
      (`decode_window`, up to 10 probes over three boundary frames, stopping at
      `CONSECUTIVE_BLANK_LIMIT`). Resolve: no flush, as the window end.
    - [ ] Emission suppression. Here: none. There: `crates/steno-speech-coreml/src/chunking.rs`
      (`emit_after_frame`) and `crates/steno-speech-coreml/src/decoder.rs` (`Hypothesis`). Resolve: goes with the
      chunker; it serves FluidAudio's warm-up window, which the VAD layout lacks.
  - Merge:
    - [ ] Overlap and anchors. Here: `crates/steno-speech/src/merge.rs` (`merge_windows`, an LCS over 1.5 s
      with tolerance `max(overlap / 2, 0.5)`). There: `crates/steno-speech-coreml/src/merge.rs` (`merge_chunks`,
      `find_contiguous_matches` with `minimum_pairs`, then `find_lcs`, over 2.0 s).
      Resolve: measure.
    - [ ] Touching windows. Here: `crates/steno-speech/src/merge.rs` (`merge_windows`, the join waits for
      the match tolerance; the LCS runs with one token a side). There: `crates/steno-speech-coreml/src/merge.rs`
      (`merge_chunks`, joins outright when `left_end <= right_start`, as FluidAudio
      does). Resolve: wait for the tolerance; the outright join keeps a word twice
      when the right repeats the left's last word a frame later (merge test
      `a_right_window_repeating_the_left_s_last_word_a_frame_later_keeps_one_copy`).
      The CoreML crate keeps the shortcut until then. `merge_chunks` also sends a
      seam with fewer than two overlap tokens on either side to `merge_by_midpoint`
      (`overlap_left.len() < 2 || overlap_right.len() < 2`), where this crate runs
      the LCS with one token a side.
    - [ ] Right window ending inside the left. Here: `crates/steno-speech/src/merge.rs` (`merge_windows`,
      the left keeps its tail, also when the right's tail only finishes the seam
      word). There: `crates/steno-speech-coreml/src/merge.rs` (`merge_using_matches`, the left's tail is
      dropped). Resolve: keep the tail (merge tests
      `a_right_window_that_ends_inside_the_left_leaves_the_left_intact`,
      `a_right_tail_without_a_word_start_leaves_the_left_its_words`).
    - [ ] Midpoint cut without a splice point on the right. Here: `crates/steno-speech/src/merge.rs`
      (`merge_by_midpoint`, the left is kept whole). There: `crates/steno-speech-coreml/src/merge.rs`
      (`merge_by_midpoint`, the right is kept from the cutoff). Resolve: keep the
      left. Keeping the right from the cutoff glues its continuation pieces onto the
      seam word and loses the left's words past the cutoff (merge test
      `a_midpoint_cut_with_no_word_start_on_the_right_keeps_the_left`).
    - [ ] Seam word. Here: `crates/steno-speech/src/merge.rs` (`merge_windows`, the left owns it unless
      its window ends inside it and the right heard more of it).
      There: `crates/steno-speech-coreml/src/merge.rs` (`word_initial_index`, `pop_seam_word`: the right owns it
      when it heard it from its start). Resolve: measure. Under a plain left-owns
      rule a left window whose tokens end inside a word, as an energy cut can, lost
      the word's rest (2,179 of 3,000 random layouts in a randomised merge check on
      #171, 0 with the exception; FLEURS unchanged; merge test
      `a_left_window_that_ends_inside_the_seam_word_takes_the_rest_from_the_right`).
    - [ ] Id matching. Here: `crates/steno-speech/src/merge.rs` (`merge_windows`, exact ids). There:
      `crates/steno-speech-coreml/src/vocab.rs` (`Vocab::ids_match`, case-insensitive). Resolve: measure.
    - [ ] LCS walk. Here: `crates/steno-speech/src/merge.rs` (`merge_windows`, forward). There:
      `crates/steno-speech-coreml/src/merge.rs` (`find_lcs`, back from the end). Resolve: measure; they differ
      only on ties.
    - [ ] Repeated words. Here: `crates/steno-speech/src/merge.rs` (`merge_windows`). There:
      `crates/steno-speech-coreml/src/merge.rs` (`find_lcs`). Both prefer a shifted alignment when a word
      repeats within the tolerance across a seam, because the shifted match is
      longer: seven "w4" 0.4 s apart merge to six. Resolve: break near-ties toward
      the smaller time offset; measure first.
    - [ ] Seam repairs. Here: none. There: `crates/steno-speech-coreml/src/merge.rs`
      (`collapse_seam_word_duplicates`) and `crates/steno-speech-coreml/src/pipeline.rs` (`repair_seam_gaps`).
      Resolve: measure once the merge is shared.
  - Recovery:
    - [ ] Gate and retry. Here: `crates/steno-speech/src/pipeline.rs` (`looks_empty`, words per second of
      speech; `recover`, the window extended only). There: `crates/steno-speech-coreml/src/decoder.rs`
      (`Hypothesis::is_whole_window_blank`) and `crates/steno-speech-coreml/src/pipeline.rs` (`should_recover`,
      an RMS check; `transcribe_window`, a perturbation ladder; `is_credible`).
      Resolve: measure on spike D's zero-token window and on FLEURS.
    - [ ] Trim of an accepted retry. Here: `crates/steno-speech/src/pipeline.rs` (`keep_chunk_and_overlap`,
      the chunk plus the overlap, a word the bounds cut kept whole). There: none,
      the retry decodes the same window. Resolve: goes with the retry; the merge
      lost a cut word's rest before the trim kept it whole (pipeline test
      `a_recovery_trim_through_a_word_keeps_the_whole_word`).
  - [ ] Chunking. Here: `crates/steno-speech/src/chunker.rs` (`layout`, VAD pause cuts with the long-pause
    skip). There: `crates/steno-speech-coreml/src/chunking.rs` (`Layout::v3`, `silence_aligned_chunk_starts`:
    14.96 s windows at a 12.96 s stride). Resolve: the VAD layout, which invariant 4
    names as shared; FLEURS passes with it here.
  - [ ] Opening marks. Here: `crates/steno-speech/src/segmentation.rs` (`OPENING_MARKS`: `¿`, `¡`, `'` and
    brackets begin a word after a boundary). There: `crates/steno-speech-coreml/src/segments.rs` (`words`,
    punctuation always glues). Resolve: the exception; "hola ¿qué" and "said
    'hello'" keep their space (segmentation test
    `an_opening_mark_with_a_boundary_begins_the_next_word`), and FLEURS did not move.
  - Names for the same concept:
    - [ ] Backend. Here: `crates/steno-speech/src/backend.rs` (`SpeechBackend`, `&mut self`, decoded to
      `Vec<Token>` by `decode_window`). There: `crates/steno-speech-coreml/src/backend.rs` (`Backend` plus
      `Scratch`), `crates/steno-speech-coreml/src/coreml.rs` (`EncoderView`) and `crates/steno-speech-coreml/src/decoder.rs` (`Hypothesis`).
      Resolve: the trait, with `Backend` and `Scratch` behind it. The decode loop's
      half is done: `TdtModel` (prediction network and joint over one window), which
      `WindowModel` implements over `Backend`, `Scratch` and `EncoderView`; the
      preprocessor and encoder calls stay with the pipeline.
    - [ ] Token. Here: `crates/steno-speech/src/decoder.rs` (`Token`: `id` a `u32`, `duration` the model's
      prediction). There: `crates/steno-speech-coreml/src/lib.rs` (`Token`: `id` a `usize`, `duration` the frames
      the loop advanced, 0 when unknown). Resolve: one type; the timings need the
      predicted duration. The shared loop emits `steno_speech::Token` and records the
      duration `DecoderConfig::token_duration` names; the CoreML crate converts it to
      its own `Token` (`From`) until its merge and segmentation take the shared one.
    - [ ] Modules. Here: `crates/steno-speech/src/chunker.rs`, `crates/steno-speech/src/segmentation.rs`. There:
      `crates/steno-speech-coreml/src/chunking.rs`, `crates/steno-speech-coreml/src/segments.rs`. Resolve: one pair of names.
    - [ ] Decoder limits. Both: the `DecoderConfig` fields in `crates/steno-speech/src/decoder.rs`
      (`max_symbols_per_frame`, `token_budget`, `window_end`, `token_duration`), which tests vary;
      `FLUID_AUDIO` in `crates/steno-speech-coreml/src/decoder.rs` sets them, the budget from
      `MAX_TOKENS_PER_CHUNK`. The fields are in place; the values follow the decode-loop
      items above.
    - [ ] Engine id. Here: `crates/steno-speech/src/engine.rs` (`OnnxSpeechEngine::ID`). There:
      `crates/steno-speech-coreml/src/engine.rs` (`ENGINE_ID`). Both are `parakeet-v3`. Resolve: one constant.
    - [ ] Swift pointers. Here: a `Swift:` line in each module doc. There:
      `(Type.method)` after each item. Resolve: one style.
    - [ ] Test tools. Here: `crates/steno-speech/src/wav.rs` (`read_pcm16`, promoted
      by WP4c) and `crates/steno-speech/tests/common/mod.rs` (`score`). There: `crates/steno-speech-coreml/src/wav.rs`, `crates/steno-speech-coreml/src/wer.rs` (`word_errors`). Resolve: one module.
  - Already the same: confidence clamping (non-finite values are zero, the rest
    clamped; one function, `decoder::confidence`, since the loop is shared), chunk and
    window.

  **WP4c sidecar and models.** `crates/steno-speech-sidecar` (binary
  `steno-speech-sidecar`) hosts the ONNX `Transcriber`; `SidecarSpeechEngine` in
  `crates/steno-speech/src/sidecar/` implements `SpeechEngine` over it.
  - Protocol (`sidecar/protocol.rs`): over the child's stdin and stdout only, never a
    socket or a file. A frame is a little-endian `u32` header length, the JSON header
    in the bridge convention (sorted keys, camelCase, a `type` tag), then the payload
    the header declares: the samples of a `transcribe` request as little-endian `f32`,
    bit for bit. Requests `load` (a store root), `health`, `transcribe`, `shutdown`;
    the child sends `ready` with the protocol version, a `memory` heartbeat with its
    resident set, and one reply per request id. A header is capped at 64 MiB and must
    start with `{`, and both readers grow their buffers as bytes arrive, so garbage
    cannot make either side reserve memory. Tests: `headers_use_the_bridge_convention`,
    `broken_frames_are_errors_not_messages`,
    `binary_garbage_is_refused_before_its_claimed_length_is_awaited` and
    `crates/steno-speech/tests/frames.rs`.
  - Limits: the parent installs the models (the child opens no connection), then
    enforces a per-request deadline (120 s plus 1 s per second of audio by default;
    300 s for `load`) and a memory ceiling (6 GiB) against the heartbeat. The client
    starts only an absolute program path, never one looked up on `PATH` or in the
    working directory (`a_program_that_is_not_an_absolute_path_never_starts`): the
    child is handed the meeting's audio. A child that dies, hangs, overruns or breaks
    the protocol is killed and reaped, the call fails with `SpeechError::Sidecar`, and
    the next call spawns and loads again; an error the child reports keeps it
    (`an_error_the_child_reports_keeps_the_child`). The child
    exits when stdin ends or stdout breaks, so a dead app leaves no child, idle or busy
    (`the_child_greets_and_exits_when_its_parent_goes_away`,
    `a_busy_child_exits_when_its_parent_goes_away`), and dropping the engine stops it
    without blocking a runtime worker
    (`dropping_the_engine_stops_its_child_inside_a_runtime_or_not`).
    `SpeechEngine::release()` stops it and frees the 2.2 GB working set
    (`requests_round_trip_the_audio_bit_for_bit_in_one_child`); the pipeline calls it
    once a job's lanes are transcribed and no other job needs the engine (see
    "Pipeline and services (WP6b)"). `prepare` downloads
    before it takes the engine's lock, so `health`, `release` and a transcription in a
    running child do not wait for a download. The deadline:
    `the_deadline_grows_with_the_audio_and_the_binary_sits_beside_the_app`. On Windows
    the child starts without a console window. ONNX Runtime's telemetry is off in
    every process that opens a session, the child included: `steno-speech`'s
    `onnx::init_environment` is the one place ONNX Runtime is configured, and
    `steno-diarize` (which depends on `steno-speech` for it) calls it too
    (`telemetry_is_switched_off_before_any_session`,
    `a_session_opens_only_once_telemetry_is_off`,
    `the_workspace_configures_onnx_runtime_in_one_place_with_telemetry_off`).
  - Platform policy (`crates/steno-speech/src/runtime.rs`): on Linux and Windows the
    sidecar is the only speech engine the app runs; on macOS the in-process CoreML
    engine is the default and the sidecar a fallback behind
    `SpeechSettings::onnx_sidecar_on_mac`.
    The in-process `OnnxSpeechEngine` is what the child hosts and what the example and
    the FLEURS test drive. Test: `the_sidecar_is_the_default_off_the_mac_and_the_fallback_on_it`.
  - Crash isolation (`crates/steno-speech-sidecar/tests/isolation.rs`, the real client
    against the real binary with `--fake-engine --fault`): killed mid-request, abort
    (the way an uncaught C++ exception in ONNX Runtime ends the process), panic, a
    panic after 1 MiB of stderr (the crash report stays bounded), exit,
    hang past the deadline, allocation past the ceiling, garbage on stdout, silence at
    start and another protocol version each end in an error and a working next call
    in a new child. On a Ryzen 7 7700 desktop, in a release build, the child loads at
    2.2 GB resident and transcribes 471 s of FLEURS German in 16.9 s, segment for
    segment equal to the
    in-process engine (`the_real_models_load_and_transcribe_in_the_sidecar_when_installed`,
    ignored by default, gated on `STENO_MODELS_DIR` and `STENO_FLEURS_DIR`, not run in
    CI).
  - Models: a file's source is a URL (GitHub release assets, 2 GB at most: Silero;
    `steno-diarize` fetches its own models, `crates/steno-diarize/src/models.rs`) or a Hugging Face repository at a
    pinned commit, `https://huggingface.co/<repo>/resolve/<revision>/<path>`, for the
    2.6 GB fp32 export (`encoder.weights` alone is 2.4 GB). `scripts/upload-models.sh`
    verifies the export against the manifest, adds the CC-BY-4.0 `ATTRIBUTION.md` and
    uploads it to `nicolaischmid/steno-models`, pinned at commit `4a133253`
    (`STENO_MODELS_REPO`, `PARAKEET_V3_FP32_REVISION`). Downloads resume
    `<name>.partial` under a file lock with `Range` requests, across retries and runs;
    a second download of the same file, in this process or another, waits for the
    lock and then finds the file installed or resumes it, so the bytes cross the wire
    once. The partial goes once its file is installed; a mirror
    (`SpeechSettings::models_mirror`) serves `<mirror>/<asset id>/<file>`. Tests:
    `crates/steno-speech/tests/download.rs` (`a_cut_connection_resumes_with_a_range_request`,
    `a_partial_a_killed_run_left_is_resumed_not_fetched_again`,
    `a_206_from_the_wrong_offset_or_without_a_range_is_not_appended`,
    `a_mirror_serves_every_file_from_asset_id_and_file_name`) and the unit tests in
    `crates/steno-speech/src/model_store.rs`
    (`a_second_download_of_one_file_waits_for_the_first_and_fetches_nothing`,
    `a_partial_is_deleted_once_its_file_is_installed_another_way`,
    `a_download_that_finds_its_file_installed_leaves_no_partial`).
  - Shipped beside the app by WP9's first half: every bundle carries the binary as a
    Tauri `externalBin` (`apps/desktop/src-tauri/tauri.release.conf.json`), checked in
    its installed layout by `apps/desktop/scripts/check-bundle.sh`.

  **WP4d diarization.** `steno-diarize`: speech-stack decision 6 and gate G3, moved
  here on 2026-10-02 so it ships with the Rust pipeline. Segmentation and embedding
  behind one backend trait (CoreML over FluidAudio's models on the Mac, ONNX Runtime
  elsewhere), Steno's clustering, timeline, mapping and refinement ported from
  `Sources/StenoSpeech/Diarization`, the G3 harness over the Forge corpus.
  **WP5 audio.** `steno-audio` from `spikes/capture-rs`: CoreAudio backend with
  device-change rebuild, synthetic backend, writer, AEC; capture tests from
  `Tests/StenoAudioTests` ported. PipeWire backend. WASAPI backend last.
- **WP6 pipeline and host.** In two PRs. **WP6a host** (`steno-host`): the view models
  and the bridge host over the real store, the service traits and fakes, the fixture
  parity suite and the ported view model tests; the parity list below is filled from it.
  **WP6b pipeline**: orchestration (`process`, retention, speaker matching, export), the
  CLI, the shell switched from its `fixture-host` feature to `steno-host` (the feature
  stays, opt-in, for UI work without a database), the real `Recorder` and `Pipeline`
  behind the host's traits. Parity: the Swift `steno export` of a calibration meeting
  equals the Rust one field for field. The shell's seams towards the host, as WP6b left
  them: the destructive alert, the folder choices, the reveal methods (the `Opener` over
  `dialogs` and `windows`) and the login item (`autostart::ShellLoginItem`) are wired;
  the prompt and its dismissal, the permissions and the update outcomes are not, for the
  reasons under "Pipeline and services (WP6b)". `[x]` Shutdown: every exit runs
  `App::shutdown` first, which stops and saves a recording in progress (`exit_request`
  and `shut_down_before_exit` in `apps/desktop/src-tauri/src/main.rs` over `ExitGate`),
  as `applicationShouldTerminate` in `apps/macos/Steno/StenoApp.swift` does (it awaits
  `AppController.shutdown`); the exits and what is still open are listed under
  "Pipeline and services (WP6b)". The keyring `SecretStore` is not the shell's: it
  lives in `steno-services` (#173, WP6b).
- **WP7 LLM, adapters, handover.** Ports of `StenoLLM` (Codex and OpenAI-compatible),
  `StenoAdapters`, `StenoHandover` (rustls, the pinned trust evaluation, the shared
  `wire.ts` contract test). Lands as three PRs: WP7a LLM, WP7b adapters, WP7c handover.
- **WP8 shell completion.** Tray, floating panels, autostart, updater,
  onboarding permissions per OS, deep links, single instance, dialogs, the six
  installer bundles, and `.github/workflows/desktop-release.yml`: a manual run that
  builds the bundles on the three platforms, unsigned, as workflow artifacts.
- **WP9 Mac cutover and signed releases.** Parity list empty, same bundle id, Sparkle
  handoff, Swift app removed, web app moved to `apps/web`, Swift rows removed from
  `AGENTS.md`; `cargo deny` with a licence allow list in CI; the signing key for the
  updater artifacts, notarisation, and the tag-triggered release workflow that
  publishes the bundles and the updater manifests. The bundles carry
  `steno-speech-sidecar` (WP4c) beside the app binary, where
  `SidecarConfig::beside_current_exe` looks: a Tauri `externalBin`, which needs the
  binary built as `steno-speech-sidecar-<target triple>` (Tauri strips the suffix
  when it bundles); on macOS it is signed with the app, with the hardened runtime,
  and notarised with it.
  In two PRs. WP9a (`feat/rust-release-signing`) did the release half: the
  sidecar in every bundle (declared in `tauri.release.conf.json`, not
  `tauri.conf.json`, so a plain `cargo build` does not need it; staged by
  `apps/desktop/scripts/stage-sidecar.sh`; `check-bundle.sh` unpacks each bundle as
  its installer would and starts the sidecar from beside the app), `deny.toml` in
  Rust CI and the release workflow, Developer ID signing and notarisation through a
  throwaway keychain as in the Swift `release.yml`, unsigned Windows installers (no
  certificate), and publishing on `desktop-v*` tags (the Swift workflow owns `v*`):
  one GitHub pre-release per tag with the bundles, the `.sig` files and
  `latest.json`, copied to the rolling `desktop-beta` and `desktop-stable` releases
  that `updater.rs` reads, each only moving forward
  (`apps/desktop/scripts/updater-lanes.sh`). No desktop release is GitHub's "latest"
  before the cutover. WP9b is the cutover: `.plans/2026-10-04-mac-cutover.md`.
  The shell's gaps that must close before the cutover (WP9b) opens; no package owns
  them yet ("Pipeline and services (WP6b)"): the tray's badge for pending speaker
  reviews; the QR encoder, a fake until a QR crate draws the pairing code; the clip
  player, a fake until WP5 adds an audio output; and the update schedule behind the
  host's `Updater`.
  The phone handover identity: on first launch on macOS the cutover either imports the
  Swift `SecIdentity` (certificate plus private key, exported from the keychain item
  `Sources/StenoHandover/Identity/IdentityKeychain.swift` writes) into the Rust PEM
  entry `handover-identity`, or accepts that phones re-pair and says so in the release
  notes; the cutover plan decides which.
- **WP10 Windows.** WASAPI capture, DirectML provider (speech-stack G4), installer.
  WP10a: WASAPI capture (#175); DirectML and the installer follow. The shell's exit on
  a Windows logoff or shutdown (`WM_ENDSESSION`, which reaches the shell as
  `RunEvent::Exit`) is untested on hardware, and Windows ends a process that has not
  answered within about five seconds, less than `SHUTDOWN_PATIENCE`, so a long save
  can be cut off ("Pipeline and services (WP6b)"); the follow-up is
  `ShutdownBlockReasonCreate` while a recording runs, so the logoff screen waits and
  says why.

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
  through the `LoginItem`, `Permissions` and `Updater` traits; WP6b implements
  `LoginItem` over WP8's `autostart`, `Permissions` and `Updater` stay the services'
  fakes (see "Pipeline and services (WP6b)").
- [x] `settings.recording.setInputDevice`, `.refreshDevices`, `.chooseFolder` (the
  shell's chooser), `.revealFolder`, `.setRetention` (Forever keeps every recording on
  disk through `Pipeline::keep_all_recordings`), `.requestPermission`.
- [x] `settings.transcription.setEngine` (rebuilds the pipeline), `.download`
  (each report the host gets publishes; `steno-services` forwards one per whole percent
  or file), `.remove`: through `SpeechModels`, which WP4 implements.
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

### Speech

- [x] The fp32 export is hosted on Hugging Face in the public repository
  `nicolaischmid/steno-models` (`STENO_MODELS_REPO`), uploaded by
  `scripts/upload-models.sh` and pinned at commit
  `4a133253481bfd2cb38dc3e77c3f748199562488` (`PARAKEET_V3_FP32_REVISION`); `prepare`
  downloads it like Silero, and where the speech sidecar runs Parakeet v3, the Settings
  download installs Silero VAD with it (the row counts as installed only with both; removing it
  keeps the VAD). A personal account, not an organisation: moving it later means a new
  upload and a new pin.
- [ ] `SpeechSettings` (`onnxSidecarOnMac`, `modelsMirror`) are Rust-only: Swift has
  neither. `steno-services` reads them from `speech.json` in the support directory
  (`steno_services::speech::speech_settings`), not from the `setting` table, which the
  Swift app rewrites whole on every save; `STENO_MODELS_MIRROR` overrides the mirror
  (the speech models only: the diarizer's models keep their hosts).
  Nothing writes the file and the bridge contract has no field for either, so the
  Settings window shows neither: the macOS fallback waits for a plan that words it for
  users, and the mirror stays configuration only.
- [ ] Where the speech sidecar runs Parakeet v3, processing a meeting before its models
  are downloaded starts a silent 2.6 GB download inside the pipeline, which the Settings
  row does not show. The same holds for a stored engine other than Parakeet v3:
  Whisper, Ultra and DE run Parakeet v3 in the sidecar, but their Settings row is their
  own and never installs, so only processing downloads the export. Either show
  pipeline-side downloads in the row of the engine that runs (and map those engines'
  rows to Parakeet v3's models), or fail processing with "Download the speech model in
  Settings" until the engine's models are installed.

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
- [x] Retention sweep at launch and after `retentionApplied`, interrupted recordings
  marked failed at launch, unfinished processing resumed at launch
  (`steno_services::App::launch`).
- [ ] Pending speaker reviews (`speakersNeedReview`): the pipeline posts the event and
  the host republishes `progress`; the tray (WP8) shows no badge for it; it must close
  before the cutover (WP9b) opens.
- [ ] Updates: Sparkle today, the Tauri updater at cutover; the `Updater` trait is still
  the services' fake, since WP8's `updater` has no automatic-check or automatic-download
  flag and no last check time to report (see "Pipeline and services (WP6b)").
- [x] Login item: registered on the first launch when the setting says so
  (`Host::register_login_item_on_first_launch`), toggled from General, the pane opened;
  the `LoginItem` trait over WP8's `autostart` (`autostart::ShellLoginItem`, WP6b).
- [ ] Calendar: the event that names a recording and its attendees, looked up at
  recording start: the recorder, WP5.
- [x] Phone pairing: a phone's arrival closing the code, a code running out, revoke,
  the listener stopping when no phone is left; the `Handover` trait, WP7 implements.
- [ ] QR encoder and clip player: fakes in the app, so the pairing code shows no QR
  image and a speaker's sample clip does not play (see "Pipeline and services
  (WP6b)").
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

### Pipeline and services (WP6b)

- The web app follows the store through a two-second poll (`App::launch`) where the
  Swift app had GRDB observation; a row written by the pipeline shows within that
  interval. Observation inside the Rust store, or a change hook on `Store::write`,
  removes the poll.
- The recorder starts without the calendar lookup (title and attendees stay the
  defaults), without the auto-stop grace after a call ends and without the meeting
  detection prompt; the detector and the capture session exist, the policy is WP5's
  and the panel WP8's.
- `STENO_MODELS_DIR` names the models directory for the app (without one in its
  settings), the CLI, the `transcribe` example and the FLEURS test alike; the ONNX
  models sit in its `onnx/` (`steno_speech::ModelStore::in_models_directory`).
- `speech_engine_id` other than `parakeet-v3` (the Swift `parakeet-ultra`,
  `parakeet-de`, `whisperkit-large-v3-turbo`) falls back to the ONNX Parakeet v3
  engine in the speech sidecar, on the Mac too; `steno dev models` lists the four
  Swift assets and can install only `parakeetV3` and `offlineDiarizer`.
- The app and the CLI build the engine through `steno_speech`'s platform policy
  (`steno_services::speech::SpeechSetup::runtime`): off the Mac, and on the Mac with
  `onnxSidecarOnMac`, Parakeet runs in `steno-speech-sidecar`, started from beside the
  running executable (`steno_services::speech::sidecar_config`); its models install
  into the models directory's `onnx/` with the mirror. In a development build
  `cargo build` at the workspace root puts the binary beside `steno` and
  `steno-desktop`; `cargo build -p steno-desktop` alone does not, and processing then
  fails with "could not start" and the path. The Settings model rows, the warm-up's
  installed check and `steno dev models` follow the same policy, so with the Mac's
  fallback on, Parakeet v3 is the fp32 export there too
  (`with_the_sidecar_chosen_parakeet_v3_is_the_onnx_export`), and Settings and
  `steno dev models list` show its size from the manifest
  (`SpeechModels::expected_bytes`,
  `the_expected_size_is_that_of_the_model_the_platform_runs`). `build()` reads the
  speech settings and the models directory once, at launch, and hands them to the
  pipeline (and every reload) and the model service, so the two agree
  (`a_reload_keeps_the_models_directory_the_app_was_built_with`); an edit of
  `speech.json` takes effect at the next launch.
- The pipeline releases the speech engine once a job's lanes are transcribed, before
  the diarizer runs, unless another job is between its warm-up and its last lane
  (`ProcessingPipeline::finish_speech`, under the warm-up's lock):
  `the_engine_is_released_after_the_last_lane_before_diarization`,
  `a_job_leaves_the_engine_loaded_while_another_still_transcribes`,
  `a_job_that_starts_during_a_release_prepares_again_after_it`,
  `a_job_that_panics_gives_its_claim_on_the_engine_back`,
  `a_job_whose_lane_cannot_be_decoded_releases_the_engine_too`,
  `a_job_whose_warm_up_fails_releases_the_engine_too`, and against the real binary
  `each_job_starts_the_sidecar_and_frees_it_once_its_lanes_are_transcribed`. So each
  job in the speech sidecar loads the 2.6 GB export again; the Mac's `CoreML` engine
  ignores the release and stays warm. A job that panics or is cancelled leaves the
  child to the next job's release or to the engine's drop. Rust only: Swift has no
  release.
- A recording's warm-up (Swift's `warmUpPipelineIfModelsInstalled`, gated on the
  same installed check) loads the speech engine only where it runs in the app's
  process, `CoreML` on the Mac, as Swift did; with Parakeet in the speech sidecar it
  loads the diarizer only (`ProcessingPipeline::warm_up_diarizer`), so no 2.2 GB
  child is resident through the recording or left without a job when the save fails.
  Both the installed check and that choice follow the engine the current pipeline was
  built with (`CurrentPipeline::current_with_engine`, set by each successful build and
  reload), not the id stored now, so neither a failed reload nor an engine the Swift
  app saved meanwhile can start the sidecar outside a job's claim
  (`a_recording_start_with_speech_in_the_sidecar_warms_the_diarizer_only`,
  `the_warm_up_follows_the_engine_a_reload_built`,
  `after_a_failed_reload_the_warm_up_follows_the_engine_the_pipeline_kept`). Rust
  only: Swift loaded the engine during every recording; with Parakeet in the
  sidecar, the job after a recording starts the child cold.
- Open, against invariant 4: the ONNX diarizer (pyannote segmentation and the
  WeSpeaker embeddings) still runs in the app's process on every platform, so a crash
  in ONNX Runtime there ends the app. Memory is not the reason to move it (the larger
  of its two models is 26 MB, against Parakeet's 2.6 GB export); crash isolation is.
  Moving it needs a request of its own in the sidecar protocol; no work package has it
  yet.
- Open: a sidecar whose parent is gone exits through `std::process::exit` from its
  heartbeat thread (`send` in `crates/steno-speech-sidecar/src/lib.rs`) while ONNX
  Runtime may still be inferring, so atexit handlers and C++ static destructors run
  beside it; a hang there would keep the 2-3 GB working set alive. `libc::_exit` on
  that path would avoid it; unverified, and no work package has it yet.
- The Swift `steno process` stamped `startedAt` from `Date()` minus the duration; the
  Rust CLI does the same to the millisecond, so a `steno export` of a CLI-processed
  meeting differs only in the ids both sides mint at random.
- The CLI's secret store is the 0600 `secrets.json` under the support directory
  (`STENO_<KEY>` wins, as in Swift); the app's is the platform keyring on macOS and
  Windows, filed as the Swift app files it: service `uno.schmid.steno.mac`, account the
  key's raw value (`crates/steno-services/src/secrets.rs` pins both against the Swift
  sources), so the Rust app reads the API key the Swift app stored. The `keyring`
  crate sets no label, where Swift wrote "Steno <key>"; lookups ignore it. The CLI and
  the app do not read each other on macOS and Windows, as Keychain and the file did
  not. On Linux the app uses the same file, so the two share it; a write is atomic
  under a lock.
- A summary re-run or a re-export the pipeline refuses (meeting busy, no LLM set up)
  is the call's error, as in Swift; one that fails after it started, a panic included,
  posts `MeetingEvent::OperationFailed`, a Rust addition (Swift awaited the call), and
  the detail shows `<operation> failed: <failure>` on its error line, as
  `MeetingDetailViewModel` did. Difference: a re-export after a speaker change that
  fails once started shows there too, where Swift kept it quiet and retried on the
  next `.ready` tick.
- No host call holds the host's lock across a network request: the probe and the
  Codex model list, also when confirming ChatGPT (Codex), run with it released, and
  the sign-in the Summaries section reads under the lock comes from the file
  (`CodexCredentialStore::stored`), as Swift's `refreshCodexStatus` read it. The
  calls still block the bridge call that made them, as Swift's awaited calls held the
  window's task.
- A meeting a previous process left recording fails at launch with Swift's "Recording
  was interrupted before it finished." (`Store::INTERRUPTED_RECORDING_REASON`).
- A recording start warms the pipeline up only when the models of the current
  pipeline's engine and the diarizer's are installed (`SpeechModels::engine_installed`),
  so it never downloads, as Swift's `warmUpPipelineIfModelsInstalled`.
- The `CoreML` Parakeet runs one call at a time, off the runtime's workers, as Swift's
  `AsrManager` actor ran it (`OneCallAtATime`).
- Settings > General acknowledges the Parakeet the platform runs: "Parakeet TDT 0.6B v3
  (int8)" from the `CoreML` repository on the Mac, as Swift; "Parakeet TDT 0.6B v3
  (fp32)" from `nvidia/parakeet-tdt-0.6b-v3` elsewhere and with the Mac's sidecar
  fallback (`SpeechModels::display_name` and `source_repo`). Open: the diarizer's
  rows still describe the Swift app's `CoreML` diarizer (its acknowledgement and its
  size in Settings > Transcription), while every platform runs the ONNX pyannote 3.0
  and WeSpeaker ResNet34-LM models; the same hooks (`display_name`, `source_repo`,
  `expected_bytes`) fix them.
- The phone intake syncs the copy and its folder to the disk before it marks the
  receipt complete (`steno_pipeline::files::copy_durably`); Swift's `copyItem` did
  not, so a power loss after the phone's 200 lost the recording on both devices. The
  receipt and meeting commits still run under `NORMAL` (the Store item below).
- Every exit runs `App::shutdown` first, once, at most ten seconds (`ExitGate`): a
  start or a stop in progress settles, a recording in progress stops with `quit` and is
  saved, the handover listener stops, and no recording starts afterwards; as Swift's
  `applicationShouldTerminate` awaited `AppController.shutdown()`, which awaited
  `awaitSettled()` first. Swift waited without a bound. The exit requests go through
  `exit_request` in the shell and are held until the shutdown ended, a second Quit
  included: Quit in the tray's menu and in the macOS menu bar (#172's own item, not
  muda's `terminate:`), a destroyed main window with no tray, the last window closing
  with no tray, and SIGTERM, SIGINT and SIGHUP on Linux and macOS (a plain `kill`,
  Ctrl-C, a closed terminal, systemd at a shutdown); a second SIGTERM or a second
  SIGINT ends the process at once, unsaved, and a SIGHUP never does; a signal the app
  inherited ignored (`nohup`, a background job's SIGINT) stays ignored. A logout on
  Linux saves when logind ends the session's processes (with `KillUserProcesses=yes`,
  systemd stops the scope with SIGTERM, then SIGHUP). Otherwise nothing signals the
  app, and when the display connection closes first, GDK ends the process unsaved;
  untested (before the first Linux release; no work package yet). Once the shutdown
  has begun, the pipeline starts no job and persists no job's failure
  (`ProcessingPipeline::quit`): a signal that reaches the speech sidecar with the app
  (Ctrl-C reaches the terminal's whole foreground group, systemd a scope's every
  process) ends its job, and the meeting stays `processing` for the next launch, as
  it did when the Swift app died with its job; the recording the shutdown saves stays
  `queued` until then. The
  Dock's Quit, a logout and a system shutdown on macOS send `terminate:` directly; tao
  answers with `applicationWillTerminate` only, which reaches the shell as
  `RunEvent::Exit` and which AppKit waits for, so the shutdown runs there
  (`shut_down_before_exit`). A logoff or a shutdown on Windows arrives the same way:
  tao answers `WM_ENDSESSION` with the run loop's end, `RunEvent::Exit`, and the
  shutdown runs there until Windows' end-session timeout ends the process: about five
  seconds, less than `SHUTDOWN_PATIENCE` (WP10). The updater's relaunch bypasses the
  exit request and runs the shutdown before it relaunches; on Windows the installer's
  own exit runs it (`on_before_exit`), and an install that fails after it ends the app
  once its message is closed. The services runtime is never dropped: dropping it
  waits, without a bound, for a transcription or a model load in progress. Open: the
  Windows logoff is untested on hardware and can outlast the end-session timeout
  (WP10), and a Linux logout saves only when logind signals the app, which is
  untested on GNOME and on KDE (before the first Linux release; no work package yet).
- The host emits under its `publishing` lock, the main thread can be waiting for a
  thread that holds it (a Stop from the tray joins the recorder's level thread, which
  publishes), and the tray's setters wait for the main thread when called from
  another. So the shell's window sink queues every snapshot and delivers it on the
  main thread, in emit order (`WindowSink` in `apps/desktop/src-tauri/src/host.rs`);
  before, a Stop from the tray froze the app.
- The host calls the recorder's commands with its lock released: the recorder reports
  each change through `Host::recorder_changed`, which takes that lock, so a failed
  start, whose permission re-read ran under it, froze the caller.
- The shell's seams WP6b leaves open, each with the package that closes it: (1) the
  detection prompt: no `DetectionController` is ported (WP5), so nothing calls
  `panels::set_prompt` and `panels::dismiss_prompt` tells no one; (2) the host's
  `Permissions` stay the services' fake (all granted): WP8's `permissions` answers
  `unknown` off the Mac and for the Mac's system audio, and the host's onboarding
  opener counts anything but `granted` as missing, so wiring it would open onboarding
  at every launch on Linux and Windows; it waits for the audio crate's tap probe (WP5)
  and a rule for what `unknown` means, which WP5 sets with the probe; (3) the host's
  `Updater` stays the fake: WP8's `updater` checks only when asked, keeps no
  automatic-check or automatic-download flag and no last check time, so the General
  section's Updates row has nothing real to show, and `updates.check` stays the
  shell's; filling it is the update schedule due before the cutover (WP9b), and the
  shell's `UpdateOutcome` stays beside the host's until then; (4) the QR encoder and
  the clip player are fakes, so the pairing code shows no QR image and a speaker's
  sample clip does not play: each needs new code (a QR crate, an audio output), not
  wiring; the audio output is WP5's; both must close before the cutover (WP9b) opens.
- The two-second pairing poll (`Host::refresh_pairing`) rides on the store poll in
  `App::launch` and runs whether or not a code is shown, where Swift ran it only while
  the Phones pane showed one.
- At `warn`, the default level, a log line carries ids, stages, counts and error kinds,
  never transcript or model text, audio, a file path or a secret; the full text goes to
  `debug` (`steno_services::log_to_stderr`). The CLI prints a failed run once, as Swift
  did.
- On the Mac, Parakeet v3 is the `CoreML` model the Swift app installs; Settings and
  `steno dev models` report its directory, and this build cannot download it.
- The audio device list is empty off the Mac, so Settings > Recording offers only the
  default input until the PipeWire (WP5b) and WASAPI (WP10) backends enumerate.
- The pipeline's `decode` reads a whole lane through symphonia (see Audio); the one
  buffer alive at a time rule holds, the buffer is the full lane.
- Ported after WP6b from #154: the room fallback. A `macCall` whose system lane holds
  under 5 % of the mic lane's speech and under ten seconds is diarized on the mic lane
  (`pipeline::diarized_lane_after_transcription`, `tap_carried_no_conversation`); the
  mic segments get clusters, no "me" speaker is made, the tap's stray segments are kept
  without a speaker (`LaneMerger::merge`'s `diarized_lane`), and fewer than two voices
  on the mic keeps it "me". A re-run that falls back removes the pipeline's "me"
  participant (`Store::delete_participant`). The handed buffer of another lane is
  dropped before the mic is decoded again, as in Swift.

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

What the Windows backend (WP10a, `capture::live::wasapi`) does differently
from the macOS one, each a parity item until a Windows machine has checked
it:

- **A Windows machine is needed** for speech-stack gate G4 (DirectML on an
  integrated GPU) and for a live capture check of the WASAPI backend: the
  `--ignored` tests in `crates/steno-audio/tests/live_windows.rs`, a real
  call recorded through process loopback, a default-device switch while
  recording, and meeting detection with Teams and Zoom. No Windows machine
  has run it; the backend is compile-tested on the `windows-latest` CI
  runner only (it has no audio device), and its device-free tests ran there
  and under wine.
- **Two clocks, no drift compensation.** The microphone and the system audio
  are two WASAPI streams on their endpoints' clocks; the Core Audio
  aggregate drift-compensates, the Windows backend does not. The system
  lane sits in a jitter buffer behind the microphone (`realtime::streams`)
  sized from the system stream's period: a target of two periods (20 ms at
  the usual 10 ms period), a slip back to the target once more than one
  period above it (counted as dropped system frames), and zeros with a
  re-prime after an underrun. `underrun_frames` counts the shortfall only,
  not the re-prime zeros that follow it. Both counts are logged when the
  capture stops. Measure the slip rate on a USB headset against built-in
  speakers; a plan decides whether to resample instead.
- **Far-end latency** is the two streams' `GetStreamLatency` less the jitter
  buffer's target. Between slips the queue drifts above the target by up
  to the high-water mark, so the echo canceller's alignment error from the
  buffer is at most high-water minus target: one period (10 ms). The
  process-loopback client may not implement `GetStreamLatency` (0 then),
  and a loopback stream's latency is not the render path's; check the
  echo canceller's alignment on hardware.
- **Process loopback scope.** Excluding Steno's process tree records every
  other process; whether that follows the default render endpoint or mixes
  every endpoint is unverified. Microsoft documents process loopback from
  build 20348; it is reported to work from Windows 10 2004, also
  unverified. The fallback, loopback of the default render endpoint (which
  records Steno's own output too), runs whenever process loopback fails for
  any reason, its 5 s activation timeout included. The user gets no notice;
  only the log says which loopback runs.
- **Default roles.** Windows keeps an `eConsole` and an `eCommunications`
  default per direction; the backend follows `eConsole` only. The
  snapshot's `output_uid` is the `eConsole` render default (the endpoint
  loopback's device), `default_output_uid` stays empty, the microphone
  default is the `eConsole` capture default, and `AudioDevices` marks the
  `eConsole` render endpoint as both the default output and the default
  system output. A change of the communications default alone costs no
  rebuild.
- **No `SampleRateChanged`.** The engine converts every stream to 48 kHz,
  so the rate never changes; a device format change invalidates the stream
  (`AUDCLNT_E_DEVICE_INVALIDATED`) and is reported as `InputDeviceGone` or
  `OutputDeviceGone`, with the same rebuild.
- **The microphone lane is the engine's mono downmix** of the capture
  endpoint (`AUTOCONVERTPCM`), not its first channel as on the Mac.
- **Detection keys on executable names.** A process "holds the microphone"
  while one of its capture sessions is active; its `bundle_id` is the image
  file name (`Teams.exe`), so the app's list of call apps needs Windows
  names. A capture session's state change is notified for sessions present
  at registration; later ones are re-registered on `OnSessionCreated` and
  otherwise caught by the detector's 1 s poll.
- **Device list.** `AudioDeviceInfo.id` is the index in the enumeration
  (WASAPI has no numeric ids), `uid` the endpoint id `Settings` stores; the
  transport type and `is_running_somewhere` are not read.

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

What the Linux backend (WP5b) does differently from the Mac's, each an
item to settle before the Linux release:

- **The system lane is the whole default sink.** The Mac's tap leaves out
  Steno's own process; the PipeWire backend records the default sink's
  monitor, Steno's output included (Steno plays nothing while recording),
  and only that sink: an app routed to another output is not in the lane.
- **Latencies are the ports' `SPA_PARAM_Latency` lower bounds**: the
  microphone port's capture side plus the sink's first playback port's
  playback side, in frames of the first cycle. Null devices report zero, so
  CI checks the parsing and the arithmetic, not real numbers. Measure the
  far-end delay on a laptop with ALSA and with a Bluetooth headset.
- **`start` waits for the first cycle** and fails after 3 s without one;
  the Mac's returns before any callback. Linking the sink's monitor keeps
  the sink running, so cycles arrive with nothing playing (the Mac's call
  mode waits for an output client). The session holds its mutex across
  `backend.start()`, so its callers, `state()` included, wait as long: 1
  to 2 s for a Bluetooth sink, against the Mac's 200 ms at most.
- **Device changes read differently.** A lost connection, stream or link
  reads as `OutputDeviceGone` (`InputDeviceGone` in person), and
  `SampleRateChanged` never fires: the adapter resamples whatever the
  graph runs at. A device destroyed and re-created under the same name and
  id (WirePlumber restarting, a USB device re-enumerated) reads as gone,
  by its `object.serial`; the defaults are forgotten while the `default`
  metadata is gone.
- **Device UIDs are `node.name`s.** A Core Audio UID saved on the Mac
  names no Linux node, so a synced or copied settings file shows the input
  device as unavailable and the user picks again. A virtual source (a null
  sink with `media.class = Audio/Source/Virtual`) records from its monitor
  output, the only output it has.
- **No input device list and no meeting detection on Linux yet.**
  `AudioDevices` (the picker) and the live `ProcessAudioActivitySource`
  (the detector) are macOS-only. The PipeWire registry holds both: the
  `Audio/Source` nodes, and the `Stream/Input/Audio` nodes with their
  `application.process.id`. A follow-up package adds them.

### Handover

Rust fixes the Swift behaviours below except the network and service name lines; each
fix is ported to Swift before cutover.

- Network, same as Swift: Rust refuses tunnels. On Linux and macOS that is every
  point-to-point interface, which most tunnels there are (`wg0`, `tun0`, `utun3`); on
  Windows every adapter but hardware Ethernet and Wi-Fi that is up
  (`advertise::windows_keeps`), which leaves out Wintun, TAP and Hyper-V adapters.
- Network, Rust differs (WP8 decides): the record is not re-published after a network
  change (restart on network change, or re-register). Layer-2 tunnels (a TAP device,
  `feth`) and bridges (`docker0`, `bridge100`) are not point-to-point, and are served;
  Swift classes bridges `.other`. A LAN numbered in `100.64.0.0/10` is refused, on
  every platform. Rust judges a connection by its local address where Swift judges the
  interface it arrives on, so on Linux and macOS (weak host model) a packet addressed
  to the LAN address that arrives over a tunnel is served: the computer is a subnet
  router or exit node, or a peer's allowed IPs cover the LAN. On Windows the hardware
  rule refuses a LAN address on a Hyper-V external switch's or a Network Bridge's
  vEthernet adapter, so a computer whose LAN address moved there is unreachable.
- Touch: `HandoverEngine.touch` should run an `UPDATE` of the row that still holds the
  token (a new `MeetingStore` method), as the Rust engine does
  (`Store::touch_paired_device`); today `store.save(seen, tokenHash:)` upserts the
  device the gate read before a yield, so a revoke in between resurrects it.
- Store reads: Swift's `HandoverEngine.sweepOrphans` and `RecordingHandler.receipt`
  read with `try?`, so a failed read counts as no receipt: the sweep deletes a
  resumable upload, a route answers 404, and an announce starts the recording over,
  overwriting a `complete` receipt so that the next `complete` admits the meeting
  twice. `HandoverEngine.authenticate` reads the device with `try?`, so a failed read
  answers 401 and the phone unpairs. Rust keeps the files and answers 500
  (`Engine::receipt`, the bearer gate).
- Pairing windows: Swift's `HandoverEngine.pair` checks only that a window is open,
  not that it is the one whose secret the head matched. The read timeout runs per
  silence, so a head whose body keeps trickling in pairs against a window opened after
  a cancel, or one opened for a second phone. A failed save gives the session back
  whenever no window is open, also after a cancel. Rust numbers the windows and pairs
  only against the one the gate matched (`Principal::Pairing`); a failed save does not
  reopen a window cancelled or replaced meanwhile.
- Revoked receipts: Swift's `HandoverEngine.persist` puts a receipt back into
  `activeReceipts` after a revoke removed it, when a `complete` that read it before the
  revoke writes it back; the receipt stream then shows an upload of a revoked phone
  until restart. Rust keeps the receipts of a device revoked since start out of memory
  until it pairs again.
- Service name: Swift's `HandoverConfiguration.defaultServiceName` uses
  `Host.current().localizedName` (the computer name in System Settings), else
  `ProcessInfo.processInfo.hostName`. The Rust default reads `HOSTNAME` or
  `/etc/hostname` and falls back to `Steno`; the shell passes the OS computer name on
  the Mac (WP9) and on Windows (WP10).

### Shell

- Launch at login is a Launch Agent through `tauri-plugin-autostart`, where the Swift
  app registers with `SMAppService`; the `requiresApproval` state never occurs on the
  Rust side. At cutover (WP9) the Swift registration has to be removed or migrated so
  the user does not end up with two login items, and the General section's copy for
  the approval state becomes unreachable.
- The menu bar on macOS carries the application, Edit and Window menus; the Swift
  Record menu (`⌘⇧R`, Record In Person) and Find Meetings (`⌘F`) are not in it yet.
- Linux on a Wayland session runs under XWayland: `main` allows GDK only its `x11`
  backend (inside the process, so nothing it starts inherits it) when
  `WAYLAND_DISPLAY` and `DISPLAY` are set and the user set no `GDK_BACKEND`, because
  GTK 3 on Wayland cannot place a window, keep it on top or report its moves, which
  the panels need. A user's `GDK_BACKEND=wayland`, or a session without XWayland,
  runs natively with panels that neither float nor keep their place; a native path
  would need the layer-shell protocol and is not planned.
- Linux shows the tray only where a status notifier host runs (KDE, most desktop
  panels, GNOME with the AppIndicator extension); elsewhere closing the main window
  quits, where the Swift `NSStatusItem` is always in the menu bar.
- Updates: Sparkle checks daily on its own (`SUEnableAutomaticChecks`,
  `SUScheduledCheckInterval` 86400 in `apps/macos/project.yml`); the shell checks only
  when asked (the tray's Check for Updates, `updates.check` from Settings).

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
- Ported after WP7a from #154: the cleanup rule that names `[index] Speaker:` as
  framing (goldens `cleanup-{de,en}.txt`) and `CleanupDraft.strippingSpeakerLabels`
  (`CleanupDraft::stripping_speaker_labels`, before validation). Rust walks grapheme
  clusters with `unicode-segmentation` as Swift's `Character` does, so the 32-cluster
  window and a colon carrying a combining mark agree.

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
| WP4a speech pipeline and ONNX backend | `feat/rust-speech` | #171 | merged |
| WP4c speech sidecar, model hosting and download | `feat/rust-sidecar` | #177 | merged |
| WP7a LLM (`steno-llm`) | `feat/rust-llm` | #167 | merged |
| WP7b adapters | `feat/rust-adapters` | #165 | merged |
| WP6a host | `feat/rust-host` | #170 | merged |
| WP5a audio (`steno-audio`) | `feat/rust-audio` | #166 | merged |
| WP4d diarization (`steno-diarize`) | `feat/rust-diarize` | #164 | merged |
| WP7c handover | `feat/rust-handover` | #169 | merged |
| WP5b PipeWire capture | `feat/rust-pipewire` | #176 | merged |
| WP8 shell completion: tray, floating panels, autostart, updater, permissions, deep links, single instance, dialogs, installer bundles and the unsigned release workflow (`cargo deny` and signing follow with WP9) | `feat/rust-shell` | #172 | merged |
| Store opens with `synchronous = NORMAL` | `fix/rust-core-concurrency-flake` | #174 | merged |
| WP6b pipeline, CLI, services, the shell on the real host, quitting saves first | `feat/rust-pipeline` | #173 | merged |
| WP10a WASAPI capture (`steno-audio`) | `feat/rust-wasapi` | #175 | merged |
| Shared TDT decoder (the decode-loop half of the WP4 integration notes) | `refactor/rust-shared-tdt-decoder` | #182 | merged |
| WP9a signed and notarised release bundles with the speech sidecar, `cargo deny`, the `desktop-v*` release and the updater lanes | `feat/rust-release-signing` | #184 | merged |
| WP9b Mac cutover (`.plans/2026-10-04-mac-cutover.md`) | | | planned |
| Services on the speech sidecar: the platform policy, the release after each job, the speech settings | `fix/rust-services-sidecar` | #183 | merged |
| fp32 Parakeet export downloads from Hugging Face (`nicolaischmid/steno-models`) | `feat/rust-host-parakeet-export` | #189 | merged |
| Every exit saves first, snapshots on the main thread, the recorder's toggle and the services runtime fixed | `fix/desktop-exit-and-deadlock` | #185 | in review |

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
decoder; PipeWire (WP5b) and WASAPI (WP10) are stubs (WP5b and WP10a
below replace them). The zero-allocation
proof is `crates/steno-audio/tests/realtime.rs`; the ERLE table is
identical to Swift's `aec-bench --synthetic`; the ring tests run under
ThreadSanitizer in CI's `tsan` job; the live Core Audio tests sit behind
`--ignored` in `tests/live.rs`. Parity items: the Audio list above.

What WP7c leaves for the next package: `crates/steno-handover` is a rustls (ring)
listener, TLS 1.3 only, hyper 1 HTTP/1.1, with the pinned verifier (`pinning`), the
rcgen identity in the `SecretStore` as one PEM bundle, pairing, the seven routes, the
inbox and the mdns-sd advertiser; `tests/wire_contract.rs` reads `wire.ts`. The store
gains the paired-device and handover-receipt queries. Core's `RecordingIntake`
(copy into the audio folder, enqueue) waits for WP6b: Rust core has no pipeline to
enqueue into yet; the audio folder's path comes from `paths::file_url_path`, the
meeting's folder from `RecordingLayout`. Durability before `complete` answers 200 is
the intake's, as in Swift: the listener fsyncs each chunk (`receiving_file::write`) and
writes its own `complete` receipt only after `HandoverIntake::admit` returns, so the
port must have the master and its commits on disk by then (the `RecordingIntake.admit`
line under Store). Pairing and revoke commits stay `NORMAL`, as in Swift: a power loss
right after one can forget a pairing (the phone gets 401 and unpairs, and the user
pairs it again) or bring a revoked device back.

WP5b is the Linux `LiveCaptureBackend`, `crates/steno-audio/src/capture/live/pipewire/`:
one PipeWire capture stream (48 kHz `f32`, one `AUXn` channel per linked
port) that Steno links itself, through the server's `link-factory`, to the
microphone's first output port and the default sink's front monitor ports.
Every graph cycle brings all lanes in one interleaved buffer, which goes
through `deliver_slices`, the safe form of the `deliver` the Mac's IOProc
calls (the view type it shares with WP10a's stream bodies); the stream's `process`
runs on PipeWire's data-loop thread. Default device moves, a node or port
going away, a failed link and a lost connection are coalesced for 500 ms
and judged with `DeviceSnapshot::difference` against the devices the
targets resolved to, as on the Mac. The proof: `tests/realtime.rs` counts
the process body on every OS, and `tests/pipewire.rs` runs against a
private headless daemon with WirePlumber and null devices
(`scripts/pipewire-headless.sh`, a step of the Linux CI job): each lane
carries its own tone, PipeWire's data-loop thread makes zero allocations
over a second of cycles, `stop()` leaves no thread and no node behind, the
device changes are reported once per burst and the rebuild's restart
runs, and changes that settle back or touch other nodes are not reported.
Linux items: the list after the Swift defects above.
WP6b is `crates/steno-pipeline`, `crates/steno-cli` and `crates/steno-services`,
and `apps/desktop` on the real host. It sits on `main` with every parent merged,
#172 included: the shell answers the bridge from `steno_host::Host` over the services
graph, each window's commands through `Host::for_window`, and Quit and a no-tray close
go through `App::shutdown`; the desktop and panel smokes pass on the real host with an
empty database.
`process` runs the Swift stage order, with progress events in core
(`MeetingEvent`, `ProcessingProgress`), learned stage rates, both intakes, the
retention sweep and the store-backed cosine memory; `steno` has every Swift command
(`capture-spike` and `audio-devices` need the Mac's live backend); one `build()`
assembles the graph; the shell's `fixture-host` is opt-in. Parakeet's ONNX engine ran
in the app's process until #183 moved it into the speech sidecar; the
`CoreML` engine leaves `language` unset (#163), and
`LanguageTaggingEngine` in the services crate runs `steno_speech`'s tagger after it,
as `ParakeetMapping` did in Swift. Secrets: the platform keyring on macOS and
Windows, the 0600 `secrets.json` on Linux (the kernel keyring does not survive a
reboot; the Secret Service, which needs D-Bus, has no work package yet).
Parity items: the Pipeline and services list above.

WP10a is the Windows half of `crates/steno-audio`: the WASAPI live backend
(process loopback excluding Steno's process tree, endpoint loopback as the
fallback, the capture endpoint, one thread per stream, endpoint
notifications and the rebuild report) and the session-based
`LiveProcessAudioActivity`. Compile-tested only: built, linted and
unit-tested on the `windows-latest` runner, no live capture on hardware.
The per-packet bodies, the stream plan and the session mapping are
platform-independent (`tests/split_streams.rs`, `tests/sessions.rs`), the
zero-allocation proof covers both stream bodies (`tests/realtime.rs`), and
the hardware checks wait behind `--ignored` in `tests/live_windows.rs`.
Parity items: the Windows list under Audio.
