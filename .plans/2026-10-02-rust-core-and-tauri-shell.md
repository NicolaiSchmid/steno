# Rust core and Tauri shell: Steno on macOS, Linux and Windows

Status: every work package from WP1 to WP10b except WP9b is merged on `main`; the port's
last pull request, #187, merged on 2026-10-05. Left: the Mac cutover (WP9b,
`.plans/2026-10-04-mac-cutover.md`, ordered and gated by
`.plans/2026-10-07-stable-promotion.md`), the first Linux release and the first Windows
release (WP10's hardware checks; the Windows installers from #184 are unsigned). "Open
after the port" lists each open item and its owner. Started 2026-10-02 on branch
`refactor/rust-workspace`. Amends `.plans/2026-09-24-initial-scope.md` (removes
"Windows, Linux" from the v1 non-goals for the next major version and replaces the
"Language / UI", "Core" and "Apps" rows of the platform table) and
`.plans/2026-09-29-macos-webview-ui.md` (its "no Electron, Tauri, Node at runtime"
decision held for the Swift host; the Tauri shell replaces that host at cutover). The
evidence is `.plans/2026-10-01-cross-platform-spikes.md` and the speech-stack decisions
and gates in `.plans/2026-10-01-cross-platform-speech-stack.md`, which this plan
executes rather than restates.

## Goal

One product on three platforms from one codebase: a Rust core, a Tauri 2 shell, and
the existing React web UI unchanged except for its transport. The Mac keeps CoreML
speech on the Neural Engine; Linux and Windows run our fp32 ONNX export of the same
model. The Swift app keeps shipping until the Rust app reaches parity on the Mac and
reads the same database, so the cutover is a download, not a migration.

Not in this plan: the rest of speech-stack WP4 (CUDA on Linux, the whisper.cpp
Vulkan engine), the iOS recorder (unchanged), and any new product feature. DirectML
on Windows is WP10b; its gate G4 stays open until a Windows machine with a GPU
measures it. The diarization rebuild (speech-stack WP3, gate G3) was outside it at
the start and moved in on 2026-10-02 as WP4d, so it ships with the Rust pipeline.
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
     asset, and `steno-diarize`'s two models (`crates/steno-diarize/src/models.rs`)
     from Hugging Face at a pinned commit and a GitHub release asset, or all of them
     from the mirror the speech settings name;
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
   socket, and opens no connection (WP4c). ONNX Runtime's telemetry is off in every
   process that opens a session (`init_environment` in
   `crates/steno-speech/src/onnx.rs`, which `steno-diarize` calls too). Until the
   cutover the Swift app keeps its own list in `AGENTS.md`.
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
  steno-adapters/          destinations (the Obsidian folder), export
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
`SecretStore` (Keychain via `keyring`, the Windows credential store over `windows-sys`;
the Secret Service over `zbus` on Linux, a 0600 file where no provider runs or the
keyring stays locked before the first move), `Updater` (Tauri updater on every
platform; Sparkle retires at cutover).

## Transition

Parallel build. The Swift app ships from `main` throughout. The Rust app is usable on
Linux first (no Swift app competes there), then on the Mac once the parity list is
empty, then on Windows once its capture backend passes the capture tests. Cutover on
the Mac is a release that ships the Tauri app as an ordinary Sparkle update, reading the
same database and settings; `.plans/2026-10-07-stable-promotion.md` gives it the new
identifier `com.nicolaischmid.steno.desktop` (its D5).

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
    the protocol during a request is killed and reaped, the call fails with
    `SpeechError::Sidecar`, and the next call spawns and loads again; one that dies or
    overruns between requests is replaced by the next call without an error (on
    `DirectML` still after a death, not after an overrun: see WP10b). An error the
    child reports keeps it (`an_error_the_child_reports_keeps_the_child`). The child
    exits when stdin ends or stdout breaks, so a dead app leaves no child, idle or busy
    (`the_child_greets_and_exits_when_its_parent_goes_away`,
    `a_busy_child_exits_when_its_parent_goes_away`), and dropping the engine stops it
    without blocking a runtime worker
    (`dropping_the_engine_stops_its_child_inside_a_runtime_or_not`). On Linux and macOS
    the child ignores SIGINT, SIGTERM and SIGHUP from its start, before its ready
    message: they reach it with the app (Ctrl-C, a closed terminal, systemd), and a
    child that died of them would end its job before the app's shutdown quit the
    pipeline (`the_signals_that_end_the_app_leave_a_request_in_the_child_answered`,
    `the_exit_signals_are_ignored_before_the_ready_message_is_written`). The client
    ends a child only with a shutdown request or SIGKILL (the memory ceiling, a
    deadline, a broken protocol); the child also ends when its stdin closes or its
    stdout breaks, as at the app's exit.
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
    panic after 1 MiB of stderr (the crash report stays bounded), exit, a reply over
    the frame limit (status 2 and a line on stderr), hang past the deadline,
    allocation past the ceiling, garbage on stdout, silence at start and another
    protocol version each end in an error and a working next call in a new child. A
    crash report waits up to 5 s for the end of the child's stderr (the wait ends when
    stderr closes), so it keeps the child's last words on a loaded machine, and those
    of a process it left behind holding stderr
    (`a_crash_report_waits_for_stderr_a_process_left_behind_still_holds`). A child
    that died or went over the ceiling between requests is replaced by the next call
    without an error (`a_child_that_died_while_idle_is_replaced_without_an_error`,
    `a_child_over_the_ceiling_while_idle_is_replaced_without_an_error`); those tests
    wait until the client's reader has queued the fault, not for a zombie pid, whose
    other threads may still hold stdout. A request the child cannot read ends it with
    status 2 and a line on stderr, with its stderr gone too
    (`a_child_that_cannot_read_a_request_says_so_and_exits_with_status_2`), and the
    ready message is the first frame even from a child that greets 200 ms late
    (`the_child_greets_and_exits_when_its_parent_goes_away`). The tests
    find the binary through `CARGO_BIN_EXE_steno-speech-sidecar`, which cargo builds
    for them, run it once by hand before any start timeout counts, and give every
    start 60 s but the silent child's (20 s). They wait for what they test (a fault
    marker, a gone pid, a queued fault, a held download) rather than for a fixed
    time. A macOS CI run on 2026-10-04 failed nine of these tests: eight children
    greeted after the 10 s start timeout the tests then had, and the silent child's
    test (5 s) read the marker of a child killed before it started, so its `NotFound`
    was the missing marker, not a missing binary. macOS checks a new executable on
    its first start (1.2 s with 16 starting at once, 0.04 s after), which on a runner
    shared with three other jobs made the starts slow; hence the warm-up run, the
    60 s timeouts and a missing marker that names itself. On a Ryzen 7 7700
    desktop, in a release build, the child loads at 2.2 GB resident and
    transcribes 471 s of FLEURS German in 16.9 s, segment for segment equal to the
    in-process engine (`the_real_models_load_and_transcribe_in_the_sidecar_when_installed`,
    ignored by default, needs `STENO_MODELS_DIR` and fails without the models,
    compares with `STENO_FLEURS_DIR` when set, not run in CI).
  - Models: a file's source is a URL (GitHub release assets, 2 GB at most: Silero)
    or a Hugging Face repository at a pinned commit,
    `https://huggingface.co/<repo>/resolve/<revision>/<path>`, for the 2.6 GB fp32
    export (`encoder.weights` alone is 2.4 GB); the diarizer's two models are an
    asset of the same store (`crates/steno-diarize/src/models.rs`, folder
    `onnx/diarization/`), the segmentation model pinned to a commit too. A mirror
    replaces every host with no fallback, so it serves a whole store root
    (`parakeet-tdt-0.6b-v3-fp32/`, `silero-vad/`, `diarization/`); the Hugging Face
    repository holds the Parakeet export only. `scripts/upload-models.sh` verifies
    the export against the manifest, adds the CC-BY-4.0 `ATTRIBUTION.md` and uploads
    it to `nicolaischmid/steno-models`, pinned at commit `4a133253`
    (`STENO_MODELS_REPO`, `PARAKEET_V3_FP32_REVISION`). The files on disk and what may
    be deleted: `model_store`'s "On disk" section; how a download runs: its
    "Downloads" section. Decided: a download holds `<name>.lock`, which no download
    deletes, while it writes `<name>.partial`, so two downloads of a file, in one
    process or two, never write the same partial; the one that waits then finds the
    file installed or resumes it, and gives up once the holder has written nothing for
    10 minutes, or for as long as a live holder's attempts may take without a byte,
    each hop of the host's redirect timed afresh (about 42 minutes for the decoder,
    which is one request, and 12 for a file of several chunks). A file over 64 MiB comes
    in `Range` requests of 64 MiB, each with a body timeout of at most 128 s, so a
    silent connection costs minutes; from the export's repository 128 MiB took 13 to
    15 s in 8 MiB chunks and 6 to 8 s in 64 MiB ones, as each request costs a round
    trip through the redirect. A mirror
    (`SpeechSettings::models_mirror`, `<mirror>/<asset id>/<file>`, the speech models
    only) should answer `Range`: a host that ignores it gets the whole file under one
    timeout for its size. Every request asks for the bytes uncompressed, as a range of
    a compressed body is no range of the file. An odd answer to a range keeps the
    partial; only wrong or surplus bytes throw it away. The lock waits run on a clock
    the tests move, and only the tests of a stalled body change the body timeouts,
    so no download test depends on the machine's speed. Tests:
    `crates/steno-speech/tests/download.rs`
    (`a_cut_connection_resumes_with_a_range_request`,
    `a_partial_a_killed_run_left_is_resumed_not_fetched_again`,
    `a_partial_longer_than_the_file_or_already_complete_is_handled`,
    `a_206_from_the_wrong_offset_or_without_a_range_is_not_appended`,
    `a_mirror_serves_every_file_from_asset_id_and_file_name`) and the unit tests in
    `crates/steno-speech/src/model_store.rs`
    (`a_second_download_of_one_file_waits_for_the_first_and_fetches_nothing`,
    `a_download_that_waited_installs_the_file_after_the_first_threw_its_partial_away`,
    `a_download_waits_while_the_holder_writes_and_gives_up_once_it_stops`,
    `a_holder_s_growth_starts_the_wait_for_a_stopped_holder_again`,
    `a_waiter_outlasts_a_holder_whose_one_request_may_still_be_silent`,
    `a_lock_this_process_holds_is_neither_locked_again_nor_opened`,
    `a_large_file_comes_in_chunks_and_a_chunk_cut_short_is_resumed_alone`,
    `a_range_s_body_timeout_follows_its_length_up_to_the_longest`,
    `a_silent_chunk_is_given_up_at_the_longest_timeout_not_its_own`,
    `a_range_follows_a_redirect_to_another_host`,
    `a_host_that_sends_less_than_a_range_asks_for_is_asked_for_the_rest`,
    `an_odd_answer_to_a_range_keeps_the_partial`,
    `a_416_to_a_resumed_range_restarts_the_file_from_zero`,
    `a_partial_is_deleted_once_its_file_is_installed_another_way`,
    `a_download_that_finds_its_file_installed_leaves_no_partial`; ignored, as it
    fetches from Hugging Face:
    `a_partial_of_the_hosted_export_resumes_through_its_redirect_in_chunks`). The engine
    installs the models on first use and outside its lock
    (`the_engine_installs_its_models_on_first_use_and_outside_its_lock`).
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
- **WP9 Mac cutover and signed releases.** The blocking list of
  `.plans/2026-10-07-stable-promotion.md` (D3) closed, the identifier `com.nicolaischmid.steno.desktop` (its D5), Sparkle handoff, Swift app removed, web
  app moved to `apps/web`, Swift rows removed from `AGENTS.md`; `cargo deny` with a
  licence allow list in CI; the signing key for the updater artifacts, notarisation,
  and the tag-triggered release workflow that publishes the bundles and the updater
  manifests. The bundles carry `steno-speech-sidecar` (WP4c) beside the app binary,
  where `SidecarConfig::beside_current_exe` looks: a Tauri `externalBin`, which needs the
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
  before the cutover. Before the first desktop release, `feat/desktop-linux-signing`
  added `SHA256SUMS` over every asset and detached OpenPGP signatures for it and the
  Linux bundles, from a key whose public half is `apps/desktop/release-signing-key.asc`
  (`apps/desktop/README.md`, "Checksums and OpenPGP signatures"); the Windows
  installers stay unsigned and the release notes say so. WP9b is the cutover:
  `.plans/2026-10-04-mac-cutover.md`.
  The shell's gaps ("Open after the port"): the tray's badge for pending speaker
  reviews and the clip player, a fake with no audio output; both follow the stable
  release (`.plans/2026-10-07-stable-promotion.md`, D3). The QR encoder and the
  update schedule, which blocked it, landed with S4 (`feat/rust-update-schedule`):
  `steno_services::qr` and `steno_services::updates`.
  The phone handover identity: on first launch on macOS the cutover either imports the
  Swift `SecIdentity` (certificate plus private key, exported from the keychain item
  `Sources/StenoHandover/Identity/IdentityKeychain.swift` writes) into the Rust PEM
  entry `handover-identity`, or accepts that phones re-pair and says so in the release
  notes; the cutover plan decides which.
- **WP10 Windows.** WASAPI capture, DirectML provider (speech-stack G4), installer.
  WP10a: WASAPI capture (#175); WP10b: DirectML for the speech encoder behind a
  probe, with the CPU as the fallback (speech-stack decision 4; gate G4 open, no
  machine); the installer follows. The shell's exit on a Windows logoff or shutdown
  (`WM_ENDSESSION`, which reaches the shell as `RunEvent::Exit`) is untested on
  hardware, and Windows ends a process that has not answered within about five
  seconds, less than `SHUTDOWN_PATIENCE`, so a long save can be cut off ("Pipeline and
  services (WP6b)"); the follow-up is `ShutdownBlockReasonCreate` while a recording
  runs, so the logoff screen waits and says why. A logoff may also end the speech
  sidecar, a console process, before `RunEvent::Exit` sets the quit latch, so its job
  could be persisted as `failed`; unverified.

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
- [ ] `SpeechSettings` (`onnxSidecarOnMac`, `directmlOnWindows`, `modelsMirror`) are
  Rust-only: Swift has none of them. `steno-services` reads them from `speech.json` in
  the support directory (`steno_services::speech::speech_settings`), not from the
  `setting` table, which the Swift app rewrites whole on every save;
  `STENO_MODELS_MIRROR` overrides the mirror, which serves the diarizer's models too, so
  it holds a whole store root. Nothing writes the file and the bridge contract has no
  field for any of them, so the Settings window shows none:
  `.plans/2026-10-07-speech-settings-ui.md` proposes their place and wording.
- [ ] Where the speech sidecar runs Parakeet v3, processing a meeting before its models
  are downloaded starts a silent 2.6 GB download inside the pipeline, which the Settings
  row does not show. The same holds for a stored engine other than Parakeet v3:
  Whisper, Ultra and DE run Parakeet v3 in the sidecar, but their Settings row is their
  own and never installs, so only processing downloads the export. Either show
  pipeline-side downloads in the row of the engine that runs (and map those engines'
  rows to Parakeet v3's models), or fail processing with "Download the speech model in
  Settings" until the engine's models are installed.
- [x] One model store: the diarizer's two models are the `steno_speech::ModelAsset`
  `diarization` (`crates/steno-diarize/src/models.rs`), installed by
  `steno_speech::ModelStore` into `<models directory>/onnx/diarization/`, the folder the
  diarizer's own store used, so no installed file moves. They get the store's lock,
  resume, ranges, progress and the mirror (`<mirror>/diarization/<file>`); the services
  build the diarizer over `SpeechSetup::model_store`, and Settings and
  `steno dev models` read the asset. Who may download is the caller's
  `steno_diarize::Install`: under `Never` a missing file is `DiarizeError::NotInstalled`
  with no request (`steno_diarize::models::installed` is the same check without a load).
  Every diarizer is on `Allowed` for now. `steno process` stays on it, since a command
  run in a terminal may download on first use; the app's `SpeechEngines` moves to
  `Never` together with the pipeline's models-missing gate (S1 in
  `.plans/2026-10-07-stable-promotion.md`), so a missing model never ends in the
  fallback while "delete after processing" removes the audio. Under `Allowed`, a
  download cut off while a meeting processes ends the job `ready` with the one room
  speaker, keeps the partial, and the next run resumes it
  (`a_diarizer_download_cut_off_mid_job_falls_back_and_keeps_the_recording`); the
  recording's retention after such a fallback is the pipeline's. Content is checked at
  download; a load that fails hashes the files, and one that fails its checksum is
  deleted and reported not installed, so Settings offers Download.

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
  recovered from their master at launch or, with none on disk, marked failed
  (`steno_services::recovery`), unfinished processing resumed at launch
  (`steno_services::App::launch`).
- [ ] Pending speaker reviews (`speakersNeedReview`): the pipeline posts the event and
  the host republishes `progress`; the tray (WP8) shows no badge for it; it follows the
  stable release (`.plans/2026-10-07-stable-promotion.md`, D3).
- [x] Updates: Sparkle in the Swift app; in the Rust app the update schedule
  (`steno_services::updates::UpdateSchedule`, S4 of
  `.plans/2026-10-07-stable-promotion.md`) is the host's `Updater` over the shell's
  Tauri updater (`updater::ShellUpdates`): a daily check, the automatic-check and
  automatic-download flags in `preferences.json`, the last check time in
  `update-check.json` (see "Shell").
- [x] Login item: registered on the first launch when the setting says so
  (`Host::register_login_item_on_first_launch`), toggled from General, the pane opened;
  the `LoginItem` trait over WP8's `autostart` (`autostart::ShellLoginItem`, WP6b).
- [ ] Calendar: the event that names a recording and its attendees, looked up at
  recording start: the recorder, WP5.
- [x] Phone pairing: a phone's arrival closing the code, a code running out, revoke,
  the listener stopping when no phone is left; the `Handover` trait, WP7 implements.
- [x] QR encoder: `steno_services::qr::PngQrEncoder` draws the pairing URL at
  error correction level M, as Swift's `CIQRCodeGenerator` did (see "Handover").
- [ ] Clip player: a fake in the app, so a speaker's sample clip does not play (see
  "Pipeline and services (WP6b)").
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
  - An API key the secret store cannot read: the Summaries section loads the rest of
    the form, shows the read error, and a save writes the key only when the field
    changed (`KeyRead`, `LlmSettingsViewModel::writes_key`), so a locked keyring's key
    is never deleted. Swift's `LLMSettingsViewModel.load` failed whole on the read, and
    its save wrote the key every time.

### Pipeline and services (WP6b)

- The web app follows the store through a two-second poll (`App::launch`) where the
  Swift app had GRDB observation; a row written by the pipeline shows within that
  interval. Observation inside the Rust store, or a change hook on `Store::write`,
  removes the poll.
- The recorder starts without the calendar lookup (title and attendees stay the
  defaults), without the auto-stop grace after a call ends and without the meeting
  detection prompt; the detector and the capture session exist, the policy is WP5's
  and the panel WP8's.
- The recorder watches each recording on its own thread (the stable plan's P18 and
  P20): a session that fails on its own (a device that stayed lost, a write that
  failed, a rebuild that panicked, which ends as a lost device) is saved and queued at
  once with `deviceLost` or `failed` and the status says why in plain words, as
  Swift's `RecordingController.observe` did (`CaptureResult::failure` carries the
  failure to Stop too, and a Stop or a quit that met a failed write or close says the
  recording may be incomplete); and the free space is read every 5 s on the volumes of
  the recordings folder and of the database, the smaller counting (`DiskWatch`,
  `fs4::statvfs`): a warning under 30 minutes of recording left and, on Linux and
  Windows, no start below 512 MiB free and a stop that saves before the disk fills
  (Rust only). A volume that reports no size or more free than its size counts as
  unreadable and never stops a recording. On the Mac a low reading only warns, never
  refusing a start or stopping a recording, and its minutes count to a full disk:
  `statvfs` leaves out APFS's purgeable space (tens of GB with local Time Machine
  snapshots), and a meeting not recorded is lost. A failed write there ends the
  recording and Steno tries to save it, but a disk that is truly full can fail the
  save too (the meeting marked failed without its asset row, or left `recording`); the
  files stay, and P3's salvage at launch (#233) and P17's asset row recover them. The
  Mac gets the floor, and the save its room, once `steno-macos` (#236, the stable
  plan's D10) reads `NSURLVolumeAvailableCapacityForImportantUsageKey`, which counts
  that space and takes `unsafe`.
- A start or a stop that panics leaves the recorder idle with an error (a start that
  panics after its meeting began also fails the meeting and removes its folder), and a
  capture start that panics leaves the session `Failed`, its backend stopped (the
  stable plan's P17, with the rebuild's panic above).
- A panic in the shell leaves `crash-<UTC time>.log` in the support directory, and one
  in the speech sidecar `crash-<UTC time>-sidecar.log`, the newest 20 kept
  (`steno_core::crash_log::install_crash_log_hook`, the stable plan's P38), since an
  app opened from the Finder or at login has no stderr anyone reads.
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
  `the_diarizer_warm_up_waits_for_a_release`,
  `a_job_claimed_while_a_finisher_waits_on_a_warm_up_keeps_the_engine_loaded` (the
  count is checked again under the lock),
  `a_finisher_that_sees_another_claim_does_not_wait_on_a_warm_up`,
  `a_job_that_panics_gives_its_claim_on_the_engine_back`,
  `a_job_whose_lane_cannot_be_decoded_releases_the_engine_too`,
  `a_job_whose_warm_up_fails_releases_the_engine_too`,
  `a_job_whose_diarizer_does_not_load_keeps_its_transcript`, and against the real binary
  `each_job_starts_the_sidecar_and_frees_it_once_its_lanes_are_transcribed`. So each
  job in the speech sidecar loads the 2.6 GB export again; the Mac's `CoreML` engine
  ignores the release and stays warm. A job that panics or is cancelled leaves the
  child to the next job's release or to the app's exit, as the sidecar engine is kept
  for the run (below). Rust only: Swift has no release.
- A pipeline reload (a Settings save of the engine or of the summaries) keeps the
  speech engine while the stored engine id runs where the current one does, and keeps
  the diarizer (`steno_services::speech::SpeechEngines`): the setup that shapes an
  engine (models directory, speech settings, sidecar binary and options) is read once
  at launch, so two engine ids share an engine exactly when `SpeechSetup::runtime` puts
  them in the same place. The claims and the warm-up's lock belong to the engine
  (`steno_pipeline::SharedSpeechEngine`), not to one pipeline, so a job on the new
  pipeline and one on the retired pipeline never release the engine under each other
  (`a_job_on_another_pipeline_over_the_engine_keeps_it_loaded_too`) nor prepare it
  during the other's release
  (`a_job_on_another_pipeline_that_starts_during_a_release_prepares_after_it`). Jobs
  across a reload queue on the one diarizer, as jobs on one pipeline do. The sidecar
  engine is built at its first use and kept for the run; it holds a child only while a
  job needs one and never runs two, also when a reload on the Mac goes to `CoreML` and
  back while a retired job transcribes
  (`a_reload_while_a_job_transcribes_keeps_one_sidecar_child`,
  `a_reload_keeps_the_speech_engine_while_the_engine_id_runs_where_it_did`). So a save
  during a recording keeps the `CoreML` model and the diarizer's models the warm-up
  loaded. The `CoreML` engine is kept only while a pipeline runs on it
  (`steno_pipeline::WeakSpeechEngine`): a reload back to it while a retired job still
  transcribes gets the same engine, and its model is freed once no pipeline holds it.
  Swift rebuilt the engine and the diarizer on every `reloadPipeline`.
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
- Closed (#185): a sidecar whose parent is gone exits from its heartbeat thread
  (`send` in `crates/steno-speech-sidecar/src/lib.rs`) while ONNX Runtime may still be
  inferring. On Linux and macOS it calls `libc::_exit`, so no atexit handler or C++
  static destructor runs beside the inference, where a hang would keep the 2-3 GB
  working set alive past the ignored exit signals; Windows keeps `process::exit`.
- The Swift `steno process` stamped `startedAt` from `Date()` minus the duration; the
  Rust CLI does the same to the millisecond, so a `steno export` of a CLI-processed
  meeting differs only in the ids both sides mint at random.
- `steno process --title` stores the title as the user's
  (`TitleOrigin::User`, #202), so the app shows it and the summary keeps it. Swift's CLI
  stores it as the default title, so the Swift app shows the date title while the
  export carries the given one, and a summary may rename the meeting; port to Swift
  only if it ships another release. Without `--title` both store the file name as the
  default title; Rust does so for a blank `--title` too, as the intake treats one.
- The CLI's secret store is the 0600 `secrets.json` under the support directory
  (`STENO_<KEY>` wins, as in Swift); the app's is the platform keyring. On macOS it is
  filed as the Swift app files it: service `uno.schmid.steno.mac`, account the key's
  raw value (`crates/steno-services/src/secrets.rs` pins both against the Swift
  sources), so the Rust app reads the API key the Swift app stored. On macOS the
  `keyring` crate sets no label, where Swift wrote "Steno <key>"; lookups ignore it.
  The CLI and the app do not read each other on macOS and Windows, as Keychain and the
  file did not.
- On Windows the app writes the credential store itself (`secrets::credentials`, over
  `CredReadW`, `CredWriteW` and `CredDeleteW`), as the `keyring` crate keeps every
  credential with the roaming profile (`CRED_PERSIST_ENTERPRISE`), where a domain
  profile's sync can remove it or bring back an older one. Each credential is kept on
  this computer (`CRED_PERSIST_LOCAL_MACHINE`), filed as the crate filed it: target
  name `<key>.uno.schmid.steno.mac`, user name the key, the value in UTF-16
  little-endian, so the crate, and a build from before, reads it too. A credential
  Steno writes carries the comment "Steno <key>"; a moved one keeps the crate's.
  `CredWriteW` replaces the credential of the same target name and type, so a
  write never deletes first. Only a credential that does not exist reads as none;
  every other failed read is an error, so the identity's load reports it unreadable
  (#221). A credential the crate wrote moves on its first read, at launch: after the
  read it is written again with local persistence and its user name and comment as
  read, under `CRED_PRESERVE_CREDENTIAL_BLOB`, so the credential manager keeps the
  blob it holds. The move carries no value, so it never writes an old one over a newer
  one, and a credential removed since the read is not found and stays removed. A
  removal wins, also one the profile's sync made in that moment. The move is read back
  and logged, and nothing is written after it. A failed move is logged and the value
  read is still used. Microsoft documents nothing about how the credential manager
  writes the move to disk, where local and roaming credentials live in different
  folders; should a power loss during that write lose the identity, the next launch
  reports the handover unavailable rather than minting a new identity (#221). No
  backup credential covers that moment: every `CredWriteW` has it, including the
  mint's, and a backup would be a second copy that a removal must also delete.
- On Linux the app keeps its secrets in the Secret Service (`SecretServiceStore`:
  attributes `service` and `username`, label "Steno <key>"):
  - The choice is made once per process, on a thread of the store's own. A read never
    asks the user; one made while a prompt is up fails, and once the choice is made
    `App::launch` builds the pipeline again, the host reads the key again (into a
    Settings form whose key field holds no unsaved edit and whose key no load or save
    wrote while it was read) and a handover that waited reads its identity again and
    starts. The crash recovery (meetings left queued or processing, unfinished exports,
    interrupted recordings) waits for the choice, so it runs on the pipeline with the
    key; a quit before the answer hands no listener over. A write may wait on the user,
    under the host's lock for Settings' save, and the first launch's mint of the
    identity on the main thread may too.
  - A keyring locked again while the app runs fails a pipeline rebuild's read; the
    pipeline then keeps the key it last read or saved (`KeepsApiKey`), also after a
    save the keyring refused, never one the user removed or changed since. A bus error
    or a provider that stops answering after the choice reads as a locked keyring.
  - The move: the first launch with a provider copies the file's entries into the
    service, reads them back and marks the file (`"movedToSecretService": true`); a
    later launch whose own connection reads every value back deletes the entries, and
    writes again any the provider lost. Before the mark, a key both hold takes the
    file's value, except the handover identity, which keeps the service's; with no
    fingerprint recorded, the file identity's is recorded first, so the handover
    reports the service's as replaced instead of adopting it.
  - After the mark the service wins for every key, and the file is no store: a key it
    lacks is an error (`KeyringUnavailable::NotOpened`), not `None`, and a write fails,
    so a run that cannot open the keyring neither mints an identity nor drops the API
    key. A write to the service drops that key's copy from the file at once, so a key
    removed or changed in the move's own launch never comes back from the copy. At a
    later launch an API key the file still holds goes when the service holds another;
    the file holds no key written after the mark, as writes fail.
  - The CLI reads `STENO_<KEY>` or the file; once the file is marked and its copy of the
    API key gone, the key reads as none, with a line on stderr, so `steno process` runs
    without summaries, as on the Mac. It reads the key only for a server endpoint.
  - A value with a line break (the identity's PEM) is stored base64 behind
    `steno-base64:`, as GNOME Keyring's unencrypted file (Omarchy's default) rejects a
    whole keyring over one.
  - Fallback and downgrade: with no provider, no default collection, a session bus that
    is not a `unix:` socket, a bus that does not answer the connection and its session
    within 2 s (zbus's method timeout does not cover the connection's set-up), or a
    keyring the user leaves locked before the first move, the app keeps every secret in
    the file for that run, shared with the CLI under the lock; after the mark the
    secrets are unavailable for that run instead. A build from before the mark cannot
    parse the marked file and fails every secret read and write (no summaries key, no
    handover) rather than minting.
  - Tested against a fake Secret Service on a private `dbus-daemon`, and against GNOME
    Keyring 50 and KeePassXC 2.7.12 on private buses; KWallet is untried (the
    manual checks are in #221).
- Handover identity guard, every platform: the identity's SHA-256 fingerprint is
  recorded outside the secret store and the settings, in `handover-identity.json`
  under the support directory (`steno.handoverIdentityFingerprint`, written
  atomically; `FingerprintFile`), and every write of the identity goes through
  `HandoverIdentity::store`, which records it. `HandoverIdentity::load_or_create`
  returns `IdentityError::Unavailable` and mints nothing when the secret store cannot
  be read, when it holds no identity while a fingerprint is recorded or a phone is
  paired, or when the identity's fingerprint is not the recorded one; the handover is
  then off for the run (an identity read while the Linux keyring asked the user is read
  again once it answered). It mints only with no phone paired, no fingerprint and no
  identity. A rollback to the Swift app leaves the file alone; a lost record is
  rewritten from the identity found while the paired phones, which the Swift app keeps,
  hold the guard. Swift keeps its own identity in the Keychain and has no guard.
- A summary re-run or a re-export the pipeline refuses (meeting busy, no LLM set up)
  is the call's error, as in Swift; one that fails after it started, a panic included,
  posts `MeetingEvent::OperationFailed`, a Rust addition (Swift awaited the call), and
  the detail shows `<operation> failed: <failure>` on its error line, as
  `MeetingDetailViewModel` did. Difference: a re-export after a speaker change that
  fails once started shows there too, where Swift kept it quiet and retried on the
  next `.ready` tick.
- No stage throws away what an earlier stage or the user produced (P10 to P13 of
  `.plans/2026-10-07-stable-promotion.md`; the template and title item is under its
  D3). Rust only, each item:
  - Speakers: a diarizer that fails or does not load, and a voice match that fails,
    cost only the speaker labels, and the stage is logged with its meeting and stage
    only. A re-run keeps the speakers stored for the meeting, confirmations, voices
    and clips included, and maps the new segments onto them by the spans of the
    stored segments each owned on the lane diarized now; an earlier fallback speaker
    alone on that lane covers the whole recording again. For a meeting with no
    stored speakers, or none on that lane (the lane diarized last time was the other
    one), the diarized lane becomes one unknown "Speaker 1" without an embedding, the
    mic lane too when it is the room, so the other party is never "me"
    (`a_failing_diarizer_keeps_the_transcript_with_one_room_speaker`,
    `a_rerun_whose_diarizer_fails_keeps_the_confirmed_speakers`,
    `a_failing_diarizer_on_the_mic_lane_makes_it_the_room`,
    `a_rerun_of_a_mic_room_whose_diarizer_fails_keeps_its_speakers`,
    `a_rerun_on_the_other_lane_whose_diarizer_fails_gives_it_the_room_speaker`,
    `a_second_diarizer_failure_keeps_the_named_room_speaker`,
    `a_job_whose_diarizer_does_not_load_keeps_its_transcript`,
    `a_failing_speaker_match_keeps_the_speakers_unknown`,
    `a_diarizer_failure_warns_with_its_stage_not_its_reason`). Sample clips
    (`crates/steno-pipeline/src/sample_clips.rs`): `diarize` writes each clip under a
    name of its run's own, `speakers/<SPEAKER-UUID>-<RUN-UUID>.wav`, and syncs it and
    the folder before the merge; a write that fails removes the files it wrote, so the
    rows keep naming the earlier clips. The merge's `replace_transcript` switches the
    rows' `sampleClipURL`s to the new files in the transaction that keeps the
    confirmations, commits durably (`synchronous = FULL`) and returns the rows it
    replaced; a confirmed speaker the run gives no clip keeps the clip and range its
    row named, when the row names one. Only then does the run sweep the meeting's own
    folder: it removes a clip file of one of the meeting's speakers (by the speaker id
    the file name starts with: one this run gave a clip or one the merge replaced)
    that no speaker row of any meeting names, matched by file name in any ASCII case.
    That takes the earlier clips and the files of a run that ended before its commit.
    A speaker the merge did not replace that this run gave no clip keeps its files in
    this run, under the rule for a dropped confirmed speaker below. So a run that ends
    at any point leaves each row naming no clip, a whole clip of the run that wrote
    the row, or, for a confirmed speaker that run gave no clip, the one its row named
    before, and a confirmed speaker's earlier clip goes only once a durable commit
    names the new one. A confirmed speaker that comes back without a clip keeps
    naming and playing its clip, which retention removes with the audio. A confirmed
    speaker the re-run drops loses its row but keeps every clip file it had, unnamed
    and not played. A later run removes such a file only if it gives the speaker id
    the file name starts with a clip, or replaces that id's row while the row is
    unconfirmed; retention removes it only when it removes the meeting's audio while
    that id is one of the meeting's speakers. Otherwise the file stays past any
    retention period until the meeting is deleted, as in Swift. The sweep runs while
    the run holds the meeting in the in-flight set, so no other run of the meeting
    has uncommitted clips in that folder, and speaker ids derive from the meeting id,
    so another meeting's clips are never the sweep's. Clips are written only into the
    meeting's own folder: a meeting whose master is not in its own folder gets
    speakers without clips and sweeps nothing. When retention removes a meeting's
    audio, it also removes the clip files of the meeting's speakers that no row
    names. No migration: the rows name the old fixed-name clips until a later run
    replaces them, or retention or a confirmation removes them by the name the row
    holds
    (`a_rerun_that_ends_at_any_clip_step_leaves_each_speaker_its_own_clip`,
    `process_again_whose_clip_write_fails_keeps_the_confirmed_clip_playable`,
    `a_runs_uncommitted_clips_survive_every_other_runs_sweep`,
    `a_meeting_whose_master_lies_in_another_meetings_folder_leaves_its_clips_alone`,
    `a_confirmed_speaker_a_rerun_gives_no_clip_keeps_its_clip_file`,
    `a_merge_into_a_confirmed_speaker_without_a_clip_keeps_its_clip_through_reruns`,
    `a_dropped_confirmed_speaker_that_comes_back_without_a_clip_keeps_its_file`,
    `a_confirmation_during_a_rerun_keeps_the_speakers_clip`,
    `retention_during_a_held_rerun_removes_nothing`,
    `sample_clips::the_sweep_removes_only_this_meetings_clips_no_row_names`,
    `sample_clips::after_the_merge_only_the_owners_unnamed_clips_go`,
    `sample_clips::a_url_in_another_case_keeps_its_clip`,
    `retention::the_sweep_removes_the_run_clip_a_confirmed_row_names`,
    `retention::the_sweep_removes_the_clips_no_row_names_of_the_meetings_speakers`,
    `confirming_after_the_audio_is_gone_removes_the_run_clip_the_row_names`). On
    Windows each write's rename and each removal is tried again while a file is busy
    (`steno_core::busy_file`). A diarizer that fails writes no clip, so the stored
    speakers come back with their clips and the sweep keeps them
    (`a_rerun_whose_diarizer_fails_keeps_the_confirmed_speakers`). Swift writes each
    clip in place at `speakers/<SPEAKER-UUID>.wav`, over the file a row may name,
    commits the merge with `synchronous = NORMAL`, and never removes the clip of a
    speaker a re-run drops; its re-run gives a confirmed speaker without a clip, the
    "me" row among them, no `sampleClipURL`. A Swift app run over this database (a
    rollback before the Mac cutover) plays the per-run files the rows name, since it
    reads each clip by its URL. Its own re-run writes `<SPEAKER-UUID>.wav` and names
    it, and drops the URL a clipless confirmed row kept; the next Rust merge then
    sweeps whichever clip no row names, that kept clip included. Once the app quits,
    a diarizer failure ends the run unpersisted, so the meeting is processed again at
    the next launch
    (`a_diarizer_failure_during_the_exit_leaves_the_meeting_for_the_next_launch`).
    Swift fails the meeting.
  - Re-run transcript: the merge's `replace_transcript` keeps a stored confirmation
    and the model's name suggestion for a speaker id that comes back, and recomputes
    the voices of the persons involved (decision 5 of
    `.plans/2026-09-29-speaker-calibration.md`;
    `replacing_the_transcript_keeps_confirmed_speakers_and_refreshes_voices`,
    `replacing_the_transcript_keeps_the_name_suggestions_of_returning_speakers`). The
    ids come from the positional "Speaker N" label, so a re-run whose clustering
    differs keeps a confirmation on whatever voice gets that label. A kept confirmation
    and the clip of the run that wrote the row switch in that one transaction (the
    sample clips above), so a confirmed speaker never plays the clip of a run that did
    not map its voice onto it; before, a clip write that failed partway left a kept
    speaker playing another voice (found: #231, #256). A kept confirmation whose new
    row has no clip keeps its earlier clip and range when its stored row names one, an
    extension of decision 5 in Rust, so the speaker plays the voice the user confirmed
    or merged onto it, from an earlier run than its embedding; the kept clip follows
    the confirmation, not the cluster
    (`replacing_the_transcript_keeps_the_earlier_clip_of_a_confirmed_speaker_given_none`,
    `a_confirmed_speaker_without_a_clip_takes_the_reruns_range`). A confirmed speaker
    whose id does not come back loses its row, as in Swift, and its clip file stays on
    the disk, unnamed and not played (see the sample clips above). Swift replaces the
    assignments and the clips and drops the suggestions.
  - Cleanup: the pass writes each segment's text by id and leaves the speakers alone,
    and summarize reads the speakers and segments as stored then, so a speaker named
    or merged during the pass stays so and is named in the summary
    (`Store::update_segment_texts`, `a_speaker_named_during_cleanup_stays_named`).
    Swift rewrites the speakers.
  - Summary: a run without a summarizer keeps the summary, tasks, decisions and name
    suggestions (`Store::save_processing_results`,
    `a_run_without_a_summarizer_keeps_the_earlier_summary`). Swift clears them.
  - Template and title: the template is the user's alone. No stage write stores
    `templateID` (`Meeting::apply_processing_results`); the host stores the pick
    before a summary re-run, and the summary's `template_id` records which template
    made it. `process` re-reads the template, title, title origin and calendar event
    before summarize, and the stages' writes keep a title whose stored origin is
    `user` (`a_template_picked_while_processing_is_kept_and_used`,
    `a_summary_rerun_leaves_a_later_pick_alone`, `a_title_typed_while_processing_is_kept`,
    `processing_results_leave_the_template_and_a_typed_title_to_the_user`). Swift
    writes back the template and title a run started with.
  - Panic: a panic inside `process` fails the meeting, named by the stage it was in,
    instead of leaving it `processing` (`a_run_that_panics_fails_its_meeting`); a
    meeting `persist` marked ready stays ready, and is delivered, whatever fails or
    panics after that write (`a_panic_after_the_ready_write_leaves_the_meeting_ready`).
    Swift has no counterpart. P13's resume guard is the crash-loop bullet under the
    services list (#228).
- An export the app was writing when it ended is finished at the next launch (P28 of
  `.plans/2026-10-07-stable-promotion.md`, amended there to retry failed exports
  too): after `resume_unfinished`, the launch re-exports, one meeting at a time, every
  ready meeting whose delivery is still `pending`, or `failed` more than a day before
  the launch or never attempted (`ProcessingPipeline::redeliver_unfinished`,
  `Store::meetings_with_unfinished_deliveries`;
  `exports_left_unfinished_are_re_exported_at_launch`,
  `launch_re_exports_a_meeting_whose_export_was_left_pending`). After three launch
  re-exports in a row that did not deliver every row it leaves the failed export
  alone, and the meeting's
  export line says "Export to <destination> keeps failing: <reason>" until the
  user's Export again, a speaker change's re-export or a summary re-run resets the
  count (`ExportRetries`, `export-retries.json` in the support directory;
  `a_failed_export_is_retried_once_a_day_and_left_after_three_failed_launches`,
  `export_again_resets_the_launch_re_export_count`,
  `a_re_run_resets_the_launch_re_export_count`,
  `launches_stop_an_export_that_fails_at_one_of_two_destinations`). A launch counts
  each re-export before it starts and saves a row the re-export left `pending`
  failed, stamped with the launch's time when it was never attempted, so an exit or
  a panic mid-export also stops after three launches
  (`launches_stop_an_export_whose_destination_panics`,
  `launches_stop_an_export_the_process_was_killed_in`,
  `a_row_failed_without_an_attempt_waits_a_day`), and for a meeting it stopped
  retrying it saves a row an exit left `pending` failed. A meeting the user exported
  while the launch worked through earlier ones is skipped when its turn comes
  (`a_launch_skips_a_meeting_the_user_exported_meanwhile`). A failed row whose last
  attempt lies after the launch's clock counts as due. A launch re-export's failure
  is logged, not shown as "Export again failed". `persist` marks the export pending
  (`DeliveryDispatcher::mark_pending`, a `pending` row per configured destination)
  before it marks the meeting ready, and a summary re-run once its summary is saved,
  so an exit before the delivery is found too
  (`the_export_is_marked_pending_before_the_meeting_is_ready`); rows that do not
  load are left as they are, so none loses its receipt
  (`marking_an_export_pending_writes_nothing_when_the_rows_do_not_load`). Rust only: Swift
  re-exported only when asked. After a rollback to Swift a `pending` row stays
  `pending`, since Swift has no launch re-export, and the audio's expiry stays
  deferred until the user exports again, as after a Swift crash mid-export; Swift
  ignores `export-retries.json`. A meeting deleted while it is re-exported can
  still get its note, as with Export again.
- The pipelines a reload builds share one in-flight set (P16; `InFlight`, as they
  share the `QuitLatch`), so the new pipeline refuses a meeting the retired one
  still processes or re-exports
  (`after_a_reload_a_meeting_the_retired_pipeline_re_exports_is_refused`). The start
  claim of `enqueue`, `reprocess` and `resume_unfinished` (#228's `claim_start`) is
  one check-and-insert on the set, held until the run ends, so a second start across
  pipelines is refused too (`a_reprocess_is_refused_while_a_retired_pipeline_runs_the_meeting`).
  Swift kept it per pipeline.
- No host call holds the host's lock across a network request: the probe and the
  Codex model list, also when confirming ChatGPT (Codex), run with it released, and
  the sign-in the Summaries section reads under the lock comes from the file
  (`CodexCredentialStore::stored`), as Swift's `refreshCodexStatus` read it. The
  calls still block the bridge call that made them, as Swift's awaited calls held the
  window's task.
- A meeting a previous process left recording is recovered at launch from its master
  on disk and queued with the end reason `failed` (`steno_services::recovery`; Rust
  only). Only when no audio folder it may be in holds its master, and the folder it
  was recorded into and the settings' folder can both be read, does it fail, with
  Swift's "Recording was interrupted before it finished."
  (`Store::INTERRUPTED_RECORDING_REASON`). Around it, also Rust only (P3 and P17 of
  `.plans/2026-10-07-stable-promotion.md`):
  - a stop's commit that finds the database busy is tried again, three tries in all
    and none more once the app quits, and a commit that still fails leaves the meeting
    `recording` for the next launch's recovery, where Swift marked it failed without
    its asset;
  - a stop whose capture failed recovers what the writer wrote from the master, where
    Swift failed the meeting;
  - a meeting left `recording` can be deleted while the recorder is idle and its
    master has not been written for ten seconds, where Swift refused every recording
    row;
  - the host reloads the meeting list when the recorder's state or meeting changes,
    where Swift's list observed the meeting table.
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
  `expected_bytes`) fix them, and the acknowledgement's licence and attribution
  (`steno_host::speech::ModelAsset::licence`) take `steno_diarize::models`'
  `LICENCE` and `ATTRIBUTION`.
- The phone intake syncs the copy, its meeting folder and the parent of every folder
  it created to the disk before it marks the receipt complete, in both apps: Rust
  through `steno_pipeline::files::copy_durably` and `create_dir_all_durably`, Swift
  after its `copyItem` (`RecordingIntake.Syncs`, `F_FULLFSYNC` with `fsync` as the
  fallback). The receipt, meeting and asset commit in one durable transaction in both
  apps (the Store item below).
- Every exit runs `App::shutdown` first, once, at most ten seconds (`ExitGate`): the
  pipelines quit, a start or a stop in progress settles, a recording in progress stops
  with `quit` and is saved, the handover listener stops, and no recording starts
  afterwards; as Swift's `applicationShouldTerminate` awaited
  `AppController.shutdown()`, which awaited `awaitSettled()` first. Swift waited without
  a bound.
  - The exit requests go through `exit_request` in the shell and are held until the
    shutdown has ended, a second Quit included: Quit in the tray's menu and in the macOS
    menu bar (#172's own item, not muda's `terminate:`), a destroyed main window with no
    tray, the last window closing with no tray, and SIGTERM, SIGINT and SIGHUP on Linux
    and macOS (a plain `kill`, Ctrl-C, a closed terminal, systemd at a shutdown). A
    second SIGTERM or a second SIGINT ends the process at once, unsaved, and a SIGHUP
    never does; a signal the app inherited ignored (`nohup`, a background job's SIGINT)
    stays ignored.
  - A logout on GNOME or Xfce runs the shutdown before the session manager lets
    the app go: the shell registers with the first session manager on the session
    bus, GNOME's `org.gnome.SessionManager`, else Xfce's `org.xfce.SessionManager`
    (the same client protocol under names of its own), finds its unique name with
    `GetNameOwner`, so it starts none, and takes the client signals from that name
    only (`apps/desktop/src-tauri/src/session_end.rs`). It answers
    `QueryEndSession` at once and `EndSession` only after the save, then quits:
    gnome-session asks before the confirmation dialog the user can still cancel
    (`CancelEndSession`), with one second to answer, and on X11 another client can
    still call an Xfce logout off after the query, after which xfce4-session sends
    the shell nothing, so the recording goes on until the end comes. gnome-session
    waits about ten seconds for the answer at the end, xfce4-session seven. `Stop`
    unregisters (`UnregisterClient`), saves and quits: xfce4-session sends it for
    Quit Program (Session settings) and kills the process 15 s later unless the
    client unregistered. It also sends it to a client it has just dropped
    (`StateChanged` to disconnected) when a checkpoint (Save Session), or a query
    whose answer it refused, has waited a minute, usually with no logout to follow,
    and with no kill; that client records on and registers again (`Phase::Dropped`),
    also after the save at a query, so a later logout still reaches it. On Wayland
    xfce4-session quits right after the query, without `EndSession` and with no
    cancel to follow, so there the shell saves at the query, answers and ends with
    the display (`SessionApi::query_ends_on_wayland`). It tells Wayland from X11 by
    `XDG_SESSION_TYPE`, else by a `WAYLAND_DISPLAY` whose socket exists; should the
    session go on 30 s after that save (an X11 session taken for a Wayland one), the
    shell tells the user and relaunches, so the user can record again.
  - Where no session manager runs (KDE Plasma, wlroots desktops), the shell opens
    the desktop portal's session monitor (`CreateMonitor` on
    `org.freedesktop.portal.Inhibit`), answers query-end at once
    (`QueryEndResponse`: xdg-desktop-portal gives about a second and Plasma 6.6's
    portal 1.5 s, and the user can still call the logout off, where a desktop may
    wait on the shell's own inhibitor) and at ending saves and quits. Plasma 6.6's
    portal serves the monitor, but nothing in Plasma 6.6 asks it, so it never
    reports the end there; Plasma before 6.6 has no monitor, and the GTK portal
    outside GNOME reports no end. While a recording runs the shell holds the
    portal's logout inhibitor (the `Logout` flag, "A meeting is being recorded")
    and closes its request when the recorder turns idle: on GNOME the GTK portal
    passes it to gnome-session, which then shows its logout dialog, even for
    `--no-prompt`; Plasma 6.6's portal records it for its session monitor, which
    nothing in Plasma asks yet; the GTK portal outside GNOME (Xfce, wlroots)
    refuses it.
  - Every logout ends the display server, and a shutdown does once logind goes
    ahead; GTK 3 then ends the process with `_exit(1)`. Each of GDK's lost-display
    paths first logs one line through GLib (X11's `gdk_x_io_error`, Wayland's event
    source), and GTK 3 logs structured, so the line reaches the process's log
    writer, synchronously, on the thread that hit the loss. The shell's writer
    (`apps/desktop/src-tauri/src/display_lost.rs`, glib's safe
    `log_set_writer_func`) runs the shutdown there, on that thread's behalf, and
    only then writes the line and lets GDK end the process; on X11 the main thread
    and tao's device thread both hit the loss and both wait for the one shutdown.
    So a save that outlasts a session manager's or logind's wait still ends, at
    most `SHUTDOWN_PATIENCE` after it began, unless the process is killed first
    (systemd's `SIGKILL` once a stop has waited out the unit's `TimeoutStopSec`,
    90 s unless the unit sets another, xfce4-session's `SIGKILL` 15 s after its
    `Stop`, which the shell calls off by unregistering first, or a second SIGTERM).
    On KDE Plasma this is the save: ksmserver speaks XSMP to X11 clients, which
    GTK 3 does not, and KWin closes only native Wayland windows at a logout, not
    the shell's, which run under XWayland.
  - A system shutdown or reboot runs the shutdown while logind waits: the shell
    holds logind's `shutdown` delay lock and releases it after the save on
    `PrepareForShutdown(true)`; logind waits at most `InhibitDelayMaxSec` (five
    seconds by default) and then goes ahead, and the SIGTERM that follows and the
    display closing both wait for the save in progress. logind has no logout
    signal, and its session `Lock` is the screen lock, which, like sleep, does not
    stop a recording. Without a session bus, a session manager, a portal or
    logind, or with the lock denied, a logout or a shutdown saves when a signal
    reaches the app or the display closes. The session clients, the portal's
    monitor and inhibitor and the lock are tested against fakes on a private
    `dbus-daemon`; the lost display under Xvfb (in CI too:
    `apps/desktop/scripts/lost-display-linux.sh`) and headless sway with a recording
    running; a real xfce4-session 4.20.4 logout on X11 and on Wayland (labwc 0.9.7),
    and its Quit Program and Save Session under a recording on X11, in a container.
  - Once the shutdown has begun, or an exit signal has arrived (the signal task calls
    `Host::quit_pipeline` before its request waits for the main thread), the pipeline
    starts no job and persists no job's failure (`ProcessingPipeline::quit`): a job the
    exit ends leaves its meeting `processing` for the next launch, as it did when the
    Swift app died with its job, and the recording the shutdown saves stays `queued`
    until then. The speech sidecar ignores SIGINT, SIGTERM and SIGHUP on Linux and
    macOS, so the signals that reach it with the app (Ctrl-C and a closed terminal
    reach the terminal's whole foreground group, systemd every process in a scope) do
    not end its job first; it exits within a heartbeat once the app is gone (see WP4c).
  - The Dock's Quit, a logout and a system shutdown on macOS send `terminate:` directly;
    tao answers with `applicationWillTerminate` only, which reaches the shell as
    `RunEvent::Exit` and which AppKit waits for, so the shutdown runs there
    (`shut_down_before_exit`).
  - A logoff or a shutdown on Windows arrives the same way: tao answers `WM_ENDSESSION`
    with the run loop's end, `RunEvent::Exit`, and the shutdown runs there until
    Windows' end-session timeout ends the process: about five seconds, less than
    `SHUTDOWN_PATIENCE` (WP10).
  - The updater's relaunch bypasses the exit request and runs the shutdown before it
    relaunches; on Windows the installer's own exit runs it (`on_before_exit`), and an
    install that fails after it ends the app once its message is closed.
  - On Linux an exit that went through ends the process two seconds later at the latest
    (`end_within` in the shell's `main.rs`), with its code; an update's relaunch is left to
    the teardown. `tauri-plugin-single-instance` 2.5 releases its bus name in its
    `RunEvent::Exit` handler, which Tauri runs before the shell's, with zbus's
    `release_name` on a connection without a method timeout, so a frozen session bus
    would hold the exit without a bound (about 20 s in #172's run, past a minute under a
    stopped private `dbus-daemon`); with the grace, the exit under that stopped bus takes
    2.2 s. The bus drops the name with the connection anyway, and the shutdown has ended
    before an exit goes through.
  - The services runtime is never dropped: dropping it waits, without a bound, for a
    transcription or a model load in progress.
  - Open: the Windows logoff is untested on hardware and can outlast the end-session
    timeout, and whether a logoff ends the speech sidecar before `RunEvent::Exit` quits
    the pipeline (which would mark its meeting `failed`) is unverified (WP10); the Linux
    logout and shutdown have not run on a real GNOME or KDE Plasma session (before the
    first Linux release).
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
  and a rule for what `unknown` means, which WP5 sets with the probe; (3) closed by
  S4: the host's `Updater` is the update schedule over the shell's updater, and
  `updates.check` stays the shell's, whose check records through the schedule; (4)
  the clip player is a fake, so a speaker's sample clip does not play: it needs new
  code (an audio output), not wiring, and follows the stable release
  (`.plans/2026-10-07-stable-promotion.md`, D3); the QR encoder, the other half of
  this seam, landed with S4.
- The two-second pairing poll (`Host::refresh_pairing`) rides on the store poll in
  `App::launch` and runs whether or not a code is shown, where Swift ran it only while
  the Phones pane showed one.
- At `warn`, the default level, a log line carries ids, stages, counts and error kinds,
  never transcript or model text, audio, a file path or a secret; the full text goes to
  `debug` (`steno_services::log_to_stderr`). The CLI prints a failed run once, as Swift
  did.
- On the Mac, Parakeet v3 is the `CoreML` model the Swift app installs; Settings and
  `steno dev models` report its directory, and this build cannot download it.
- The audio device list comes from the live backend on every platform
  (`PlatformAudioDevices`: Core Audio, the PipeWire registry, WASAPI); a failed
  enumeration is logged, and Settings shows Swift's "Microphones could not be listed."
  over an empty list. Rust differs: a chosen microphone the list lacks (unplugged, or a
  UID saved on another computer) stays selected and is listed as "Microphone not
  connected" (`recording_devices` in `steno-host`), where the Swift picker shows no
  entry for it.
- The pipeline's `decode` streams a lane (see "Streamed decode and mixdown" under
  Audio); the one buffer alive at a time rule holds, and the buffer is the 16 kHz lane
  (460 MB for two hours) plus a working set under a megabyte, as in Swift.
- Launch recovery stops resuming a meeting whose processing keeps ending the app
  (`crates/steno-pipeline/src/crash_loop.rs`); Swift resumed every unfinished
  meeting at every launch. What is counted: each background run adds one to
  `.processing-runs` in the meeting's folder before it starts. A run that ends while
  the app lives (ready, failed or panicked) clears the count, earlier crashes
  included; the exit takes each open run back off, one each, on the current pipeline
  and the retired ones, so earlier crashes still count. What is left is the runs that
  ended with the app: an abort, an out-of-memory kill, a power loss. Who is charged:
  every run alive at a crash, so launch recovery runs a meeting with a count alone,
  after the others and oldest first, and counts it only when its turn comes (a run
  started at once that never ends holds them back, uncounted, for the session, and
  the next launch resumes them). When it gives up: after three such runs,
  `resume_unfinished` marks the meeting failed and clears any retention stamp, so
  the sweep keeps the audio. What the user sees: "Steno closed unexpectedly 3 times
  while processing this recording and stopped trying; the recording is kept." How
  to start over: `enqueue` or `reprocess` clears the count. A file, not a column, so
  no migration; it is kept only in a folder named after the meeting's id, so two
  meetings never share one.
- "Process again" (P9 in `.plans/2026-10-07-stable-promotion.md`): its pipeline
  entry point, `ProcessingPipeline::reprocess`, is refused with a typed
  `ReprocessError` when the meeting is unfinished or busy, its master is gone, or
  the app is quitting; it drops the retention stamp an earlier run left, so a retry
  that fails keeps the audio, and the retention sweep checks each asset again in the
  write that removes its files, so a meeting queued after the sweep read its list
  keeps its audio. The bridge method `meeting.processAgain` (no params, on the
  selected meeting) is answered through the host's `Pipeline::process_again`, whose
  `ProcessAgainRefusal` the detail words on its error line (nothing while the app
  quits). The host decides when it is offered: the detail snapshot's
  `canProcessAgain`, `Meeting::offers_process_again` (a failed meeting today) and the
  master on disk, and the pipeline's `process_again` checks the same rule under its
  own read, so a stale detail cannot run a ready meeting again. The Swift app sends
  `canProcessAgain: false` and refuses the method in words. `steno process --meeting
  <id>` is Rust only and takes a ready meeting only with `--allow-ready`.
- Ported after WP6b from #154: the room fallback. A `macCall` whose system lane holds
  under 5 % of the mic lane's speech and under ten seconds is diarized on the mic lane
  (`pipeline::diarized_lane_after_transcription`, `tap_carried_no_conversation`); the
  mic segments get clusters, no "me" speaker is made, the tap's stray segments are kept
  without a speaker (`LaneMerger::merge`'s `diarized_lane`), and fewer than two voices
  on the mic keeps it "me". A re-run that falls back removes the pipeline's "me"
  participant (`Store::delete_participant`). The handed buffer of another lane is
  dropped before the mic is decoded again, as in Swift.

### Store

- `SettingsStore.save` should write and delete only the keys `Settings` knows and
  keep the fields of `ObsidianSettings` it does not know, as the Rust store does;
  today it deletes every `setting` row first, so a save drops what a newer build wrote.
- `StenoJSON` date output truncates to the millisecond; Rust rounds like GRDB; fix the
  Swift formatter before cutover.
- An applied migration a build does not know (a newer build's) is ignored in both
  apps: GRDB's migrator never reads an identifier it was not given and
  `MeetingStore.init` does not ask `hasBeenSuperseded`; the Rust migrator logs a
  warning and goes on (`migrate` and `check` in
  `crates/steno-core/src/store/migrator.rs`), and an unknown identifier does not
  count towards the known ones applied. That is safe because a migration only adds
  tables and columns (`.plans/2026-10-07-stable-promotion.md`, "Migrations add,
  never change"). The Rust desktop shows a dialog and exits when the store, or
  anything else the host needs, cannot be opened at launch (`Refusal::Unavailable`
  in `apps/desktop/src-tauri/src/main.rs`), where it panicked before.
- Schema v5 adds the handover admission ledger, `handoverAdmission` (recording id,
  byte count, SHA-256, meeting id, admitted at; no foreign key), in both apps
  (`.plans/2026-10-08-handover-admission-ledger.md`). The admission transaction
  below writes its row, or moves a row whose meeting was deleted to the new
  meeting; every open backfills it from
  `complete` receipts whose meeting exists (`Store::backfill_handover_admissions`,
  run by `Store::open` and `Store::in_memory` but not `open_without_migrating`;
  `MeetingStore.init`, the same SQL), which covers admissions before v5 and those an
  older app commits during a rollback. A revoke and a meeting delete leave it
  (`Store::admitted_meeting`, `MeetingStore.admittedMeeting`).
- `RecordingIntake.admit` commits the `complete` receipt, the meeting and its asset in
  one transaction, on the disk before the computer answers `complete`, because the
  phone then deletes its copy: `Store::save_admission_durably`,
  `MeetingStore.saveDurably(_:meeting:asset:)`. No crash, full disk or busy store can
  leave a `complete` receipt without its meeting; earlier releases committed the two
  separately, and the engine reads such a receipt as not admitted (the Handover
  "Admission" item). Only then does the intake delete the upload and hand the saved
  meeting to the pipeline (`ProcessingPipeline::enqueue_saved`, `enqueueSaved`, which
  write nothing); a pipeline that cannot take it leaves the meeting `queued` for the
  next launch's resume, the admission stands, and both apps log the failed enqueue. A
  failed commit is not proof that nothing committed: when the WAL sync at the commit
  fails, its frames stay in the WAL and recovery after a crash replays them. So the
  intake saves the receipt `failed` durably first
  (`Store::save_handover_receipt_durably`, `MeetingStore.saveDurably(_:)`), whose
  commit writes over those frames or voids them, and removes the copy only once that
  save succeeds; otherwise the copy stays, an orphan at worst, and a replayed
  admission still finds its master. The phone keeps its copy either way. Orphan
  masters are not cleaned up or offered for re-import yet (the
  `fix/recovery-adopts-orphans` item in "Open after the port"). The intake completes
  only a receipt of the admitting upload: its read and, again, the admission's
  transaction refuse another device's receipt under the same recording id, and one
  of other bytes (`StoreError::ReceiptOfAnotherUpload`,
  `MeetingStoreError.receiptOfAnotherUpload`), and leave that receipt as it is, the
  copy removed. When the ledger already holds the bytes and their meeting exists,
  the transaction completes the receipt with that meeting and writes no other, and
  the intake removes its copy and enqueues nothing: the same bytes are one
  recording. The intake's writes run outside
  the engine's line of store writes, so between its read and its commit the admitting
  phone can be revoked and another phone announce the id; completed, that phone's
  receipt would answer its `complete` with this meeting and it would delete a
  recording never admitted. Every production wiring goes through this intake
  (`RecordingIntake::over`, `steno_services::app::handover_intake`, Swift's
  `RecordingIntake.init(currentPipeline:)`, which `AppEnvironment.makeIntake` and
  `init(pipeline:)` go through).
- The pairing's save and the revoke's delete are durable too
  (`Store::save_paired_device`, `Store::delete_paired_device`, Swift's
  `save(_:tokenHash:)` and `delete(deviceID:)`): the phone keeps the token the pairing
  answered with, so a power loss must not forget the pairing, nor bring a revoked
  phone back. Every other write, the last-seen touch included, stays `NORMAL`.
- A durable transaction runs under `synchronous = FULL` with `fullfsync` on
  (`F_FULLFSYNC` on Apple platforms also flushes the drive's cache), so its commit
  syncs the WAL, and the writer goes back to `NORMAL` once it has ended, on every path,
  in the same hold of the writer: `Store::write_durably` (a guard that restores on
  errors and panics), `MeetingStore.writeDurably` (one `writeWithoutTransaction`, a
  `defer`). Tests read the level and the rows inside every commit (Rust's
  `Store::probe_commits` behind `testing`, Swift's `CommitLog` over the writer's
  trace), make the admission's commit fail with a trigger, the `failed` save too, and
  check that the `failed` receipt commits under `FULL` while the copy is still there;
  a power loss itself is not tested, nor a failing WAL sync (that needs a
  fault-injecting SQLite VFS; the probe in #213's description ran one outside the
  repository). The Rust store also turns `checkpoint_fullfsync` on (`set_up` in
  `crates/steno-core/src/store/mod.rs`, shared by `Store::open` and
  `Store::open_without_migrating`), which the bundled SQLite leaves off and Apple's
  system SQLite under GRDB has on, so a checkpoint on a Mac cannot undo a durable
  commit.
- At launch, before the handover listener exists, both apps checkpoint the store
  durably (`Store::checkpoint_durably`, `MeetingStore.checkpointDurably()`):
  `wal_checkpoint(TRUNCATE)` under `FULL` with `fullfsync` copies every commit in the
  WAL into the database file, syncs it, waits until no reader is left in the WAL and
  truncates the WAL file, keeping a new salt for its next header; one durable write
  that changes a page (a private table created and dropped, which leaves the schema
  and the applied migrations as they were) then writes that header and syncs it with
  its frames. After a crash, recovery can read back an admission whose WAL sync failed
  (Linux keeps a page whose fsync failed in its cache, marked clean), and the intake
  answers a retried `complete` from a stored `complete` receipt without a write of its
  own; the checkpoint puts that admission in the synced database file first. The
  truncation itself is not synced; the synced header is what counts, since recovery
  replays only the frames under the header's current salt, so a power loss replays
  none of the older frames over the checkpointed pages. A checkpoint that fails, or
  that another connection (a writer, or a reader still in the WAL) blocks when the
  busy timeout runs out (`SQLITE_BUSY`), keeps the handover off until the next launch,
  with a startup warning (the Mac app's menu bar, the Rust shell's log) and Settings'
  unavailable callout; the rest of the app runs (`handover_listener` in
  `crates/steno-services/src/app.rs`, `AppEnvironment.makeHandover`). Tests check at
  the file level that the WAL file holds only its header and the write's frames and
  that its salt changes, that the write after the checkpoint commits under `FULL`, and
  that another connection's write lock or read transaction makes the checkpoint fail,
  and with the write lock held expect no listener and no identity read. The launch
  checkpoint is one helper, `HandoverService::checkpoint_store` and
  `HandoverService.checkpointStore(_:)`, which fails with `StoreNotSynced`; the CLI's
  `steno dev handover serve` in both apps runs it before it mints the identity and
  exits nonzero when it fails. The CLI's store is in memory, so its checkpoint has no
  WAL to copy and no test can make it fail; the helper is tested through the apps.
- On Windows a folder flush alone does not make a rename durable (the FAT driver treats
  a flush of a folder other than the drive's root as a no-op), so the durable writes
  (`steno_pipeline::files`: `replace_file`, `copy_durably`) rename with `MoveFileExW`
  and `MOVEFILE_WRITE_THROUGH`, which returns only once the rename is on the disk. Where
  that call fails (a target another handle holds open), they rename with std. Either way
  they then flush the renamed file, which on NTFS commits the journal that holds the
  rename and the folders created before it, and on FAT32 also flushes every folder above
  the file. std's rename and the reopen for that flush try again when another handle
  holds the file for a moment (a sharing or lock violation, or "access denied" on a
  target such a handle holds open or on a file being replaced or deleted that instant),
  after waits of 5 ms doubling to 200 ms, about 0.9 s in all; two writers of one path
  (spelled the same) in the process rename and flush one after the other, so one
  writer's flush does not make another's write fail. The vault writer, the handover
  inbox, the Codex sign-in file, the model downloads and the speaker clips share these
  retries through `steno_core::busy_file`. A flush that fails is an error, so
  the phone intake answers 500 and the phone keeps its copy. `create_dir_all_durably`
  flushes the parent of each folder it creates, and a durable write flushes the folder
  it renamed into (`FlushFileBuffers` on the folder). A folder that does not open, or a
  folder flush that fails to write, is an error; a drive that refuses the folder flush
  ("access denied", "not supported" and the like, from some network shares) is logged
  and passed over, as a failed folder sync is on Linux and macOS. Neither the FAT
  driver's folder flush nor exFAT's unpublished driver can be relied on, and a server
  can acknowledge a flush without writing it, so Settings warns under an audio folder on
  a drive that is neither NTFS nor ReFS, or on a network drive (a share's path, a drive
  letter mapped to one, or a folder that resolves to a share), with "This drive may lose
  recent recordings in a power cut." and "Choose a folder on your computer's main drive
  to keep them safe." CI's Windows job runs the intake's writes on a FAT32 and an exFAT
  drive. The intake creates the meeting folder itself (`create_new_dir_durably`), so a
  folder already at its path (an id that collided) fails the attempt before the copy and
  is left as it is; an attempt whose folder sync or copy fails removes the folder it
  created. The Swift intake accepts a folder already there and replaces a file at the
  master's path, reachable only by a UUID collision; it removes the copy whose sync
  fails and leaves the empty folder. Linux and macOS sync the folder after std's rename,
  best effort, and Settings shows the same warning under an audio folder on a network
  mount, read with `statfs` (NFS, SMB, VM host shares, remote FUSE and macFUSE;
  `files/mount.rs` names them). The Swift app has no such warning; it ships no further
  release. The adapters' `AtomicFileWriter` flushes the renamed file on Windows as well,
  and a failed flush fails the export: with "delete after processing" the vault's copy
  of the mixdown is the only audio left once the sweep has run.

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
- `ObsidianFolderDestination.deliver` should run one delivery per vault at a time, as
  the Rust destination does with its per-vault lock: `DeliveryCoordinator` is an
  actor but re-entrant at `await destination.deliver`, so two meetings finishing at
  once can drop one meeting's line from a shared person page, pick one folder for two
  first deliveries with the same slug, and sweep each other's temp files. A first
  delivery should claim its folder by creating it without intermediate directories,
  a folder that is already there counting as taken unless it holds this meeting's
  `meeting.json`, as the Rust `claim_folder` does. Between the two apps, or two CLI
  runs, on one vault only the folder claim holds; the person-page merge and the temp
  sweep are not guarded across processes in either app.
- `removeMeetingLine` should skip a listed page that is the same file as a page this
  delivery rendered, as the Rust destination does (device and inode on Unix and the
  final path name on Windows; Swift can compare `fileResourceIdentifier`): on APFS a
  person renamed from `anna` to `Anna` today loses this meeting's line from the one
  page both names refer to.
- A redelivery should check whose `meeting.json` the pinned folder holds before it
  writes there, as the Rust destination does: when the user moved or deleted the
  meeting's folder and a meeting with the same date and title claimed the name since,
  Swift writes over that meeting's notes. The Rust destination claims and writes a
  folder as a first delivery would (`X-2`) and drops the old folder's files from the
  receipt. It does the same when the pinned folder's `meeting.json` names no meeting,
  or is missing and neither the `transcript.vtt` header nor the folder note's
  `steno_id` names this meeting: a duplicate folder, never an overwrite. A
  `meeting.json` or note that cannot be read fails the delivery. A `meeting.json` the
  user deleted while the notes still name this meeting is written back in place, as
  in Swift. A pinned folder that is gone is claimed again by creating it (a dangling
  symlink counts as there), and a folder a delivery created is removed again when it
  is still empty after the delivery failed. When that removal fails (on Windows a sync
  client can hold the folder open), the empty folder reads as taken and the next
  attempt claims `X-2`; this is accepted. With person pages off, the meeting's lines
  on its listed pages keep linking the old folder's note until a delivery with person
  pages on.
- When the audio copy is on, a phone recording (`M4aAac`) without a mixdown is copied
  from its own AAC file; Swift fails it with `audioUnavailable`. When the audio copy
  is on, the file it copies (the mixdown, or a phone recording's AAC file) is gone
  and the meeting folder holds no audio file on disk (a receipt entry for one that
  is gone is dropped), the Rust destination writes the
  notes and returns a warning in the receipt ("The audio was already removed, so the
  export has no audio file"), which the meeting's export line and `steno deliver`
  show. Swift fails the delivery with `audioUnavailable` after writing every other
  file, so its export stays failed on every retry. The Rust `ObsidianError` has no
  `AudioUnavailable`.
- On Windows the Rust person pages keep off the reserved device names (`Con` becomes
  `Con_.md`, linked `[[Con_|Con]]`); the Swift app runs on the Mac only, where the
  names are allowed, so it has nothing to mirror and both apps name a Mac page `Con.md`.

### Audio

What the audio crate (WP5a) does differently from `StenoAudio`, each a
parity item until a plan says otherwise:

- **Mixdown is 16 kHz mono Int16 WAV, not AAC.** There is no AAC encoder in
  pure Rust; `SymphoniaAudioCodec::mixdown_format()` says `Wav16kInt16` so
  the persist stage names `audio.wav` correctly. Options at cutover: ship a
  small AAC encoder (`fdk-aac` is non-free; `ffmpeg` is too large), accept
  WAV for the optional export, or encode through the platform (AudioToolbox
  on the Mac, Media Foundation on Windows) behind a `cfg`.
- **Streamed decode and mixdown: parity.** A CAF master is read 32 768 frames
  at a time, as the AVFoundation codec converted, other files a packet at a
  time, and each block is resampled as it arrives (`LaneResampler`); a rate or
  channel count that changes mid-file keeps all of the file, as Swift's one
  `AVAudioFile` decode did. The whole-file decoder before it dropped the
  samples before a channel-count change and resampled all of a rate-only
  change at the last rate. Bounds and measurements: A1 in
  `.plans/2026-10-07-stable-promotion.md`, and #228.
- **A sidecar is taken only when it is the master's length at 16 kHz.**
  Swift's codec took any non-empty sidecar. The Rust `decode` reads the
  master CAF's frame count from its header and decodes the master when the
  sidecar disagrees with it: the sidecar's 32-bit size fields wrap after
  37.3 hours (a wrapped header can still parse and claim a short lane), and
  a sidecar that missed the master's last frame on a full disk would lose
  it. A master that then fails to decode falls back to that sidecar, and a
  master that is not the writer's CAF leaves the sidecar trusted.
- **Resampling.** 48 kHz masters go through the writer's exact 3:1 FIR
  with its group delay dropped, so the decode is zero-phase on the master's
  time; other rates (the phone's 44.1 kHz) through a 64-tap, 128-phase
  windowed sinc. The Swift codec used `AVAudioConverter` at maximum
  quality; the two are not bit-identical. The 3:1 FIR is flat to 7 kHz;
  the sinc, measured in `crates/steno-audio/tests/codec.rs` from 44.1 kHz,
  is within 0.3 dB to 6 kHz and -1.3 dB at 6.5 kHz, with 12 kHz aliasing
  below -50 dB. A sweep (`tests/resampler_sweep.rs`) puts every alias that
  lands below 7 kHz under -60 dB and folds 8 to 9 kHz into 7 to 8 kHz at
  -21 to -58 dB; the capture's path for a 44.1 kHz device (the
  `RateConverter`, then the 3:1 FIR) stays under -60 dB below 8 kHz.
  FLEURS German through the 44.1 kHz path gives 5.02 % WER, through the
  48 kHz path 5.51 % (`steno-speech`'s `tests/fleurs.rs`, model gated);
  FLEURS is 16 kHz, so that proves the passband, not the fold. Final, the
  fold included: Nicolai accepted it on 2026-10-08 (stable plan D9 and A9).
- **The sidecar lags the master decode by one group delay.** The live
  16 kHz sidecar is the same FIR run causally, so its onset sits 32 samples
  (2 ms) after the master decode's; `decode` prefers the sidecar, so a
  transcript's timestamps shift by 2 ms depending on which file was read.
  Swift had the same relationship (causal sidecar writer, delay-compensated
  `AVAudioConverter`); `tests/codec.rs` pins both onsets and the 32 samples
  between them. Accepted (stable plan D9).
- **AAC priming is trimmed: parity.** AVFoundation dropped the encoder's
  priming samples; symphonia 0.5 trims them for MP3 (the LAME tag, with
  `enable_gapless`, measured: onset at sample 2 of an ffmpeg encode) but
  parses and ignores the MP4 edit list and does not read `iTunSMPB`. The
  decoder finds the priming itself (`codec::priming`: the sound track's
  `elst` media time, ffmpeg's; else the `iTunSMPB` tag, `AVAudioFile`'s
  2 112 samples; else the 2 112 AVFoundation assumes, but only in the
  layout of the phone's `AVAudioRecorder`, whose files declare neither; any
  other file that declares neither keeps every sample) and drops exactly
  that many frames by packet timestamp, so an AAC lane starts on the
  encoder's first sample (`Tests/Fixtures/audio/tone-440-44k1-onset-200ms.m4a`
  lands on its onset sample; `tone-440-44k1-onset-200ms-apple.m4a`, Apple's
  encoder in the recorder's layout, on AVFoundation's). Only AAC in MP4
  changes; the padding after the last sample stays (under a packet of
  silence). One difference on purpose: AVFoundation also cut 2 112 samples
  from a file of another encoder that declares no priming (Android's
  `MPEG4Writer` primes 1 024), and the Rust decode keeps them (stable plan
  D9).
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
- **A chosen microphone that is missing or cannot be opened no longer ends
  a recording while the default input can be opened**, a deliberate parity
  change under the rule that no recording is lost. Swift fails the start
  with `InputDeviceUnavailable`, and a microphone lost during a recording
  ends it in `DeviceLost` after the rebuild's four restarts. Swift keeps its
  behaviour until the cutover.
  - **Missing.** Every live backend (Core Audio, PipeWire, WASAPI) records
    the default input in its place (the fallback,
    `CaptureInput::is_fallback`), at `start` and at a rebuild's restart
    alike (`CaptureBackend::start`), logs one warning, and names the device
    in `CaptureStream::input`.
  - **Connected but does not open** (still settling after it was plugged
    in, held by another app, a link that never runs). The backend's `start`
    fails; the session then starts the backend once more without a UID, at
    the start and after a rebuild's last restart, and marks that input as
    the fallback.
  - **The gap.** A rebuild tries the default already after its first failed
    restart when the stream it replaces was on the fallback (the default
    worked a moment ago), or after the first restart on which the graph did
    not run (`CaptureError::DidNotRun`, PipeWire's 3 s first-cycle
    deadline). The gap is then one failed start long (about 3 s of wall time
    on PipeWire), within the 10 s a gap is filled with silence
    (`MAXIMUM_GAP`), so the restarts take nothing from the master. Waiting
    out all four restarts would cost about 14 s on PipeWire, 4 s of it
    missing from the master.
  - **The warning.** The recorder sets the recording snapshot's `warning` to
    "Recording from <name>. The microphone chosen in Settings is not
    available." (or "from the system default microphone" when the input has
    no name) until a rebuild returns to the chosen one, the user dismisses
    it or the recording stops. A recording that was on the fallback at any
    point, unless the warning was dismissed, leaves "Steno recorded from
    <name> while the microphone chosen in Settings was not available."
  - **The return.** Each backend watches for the chosen device (the Mac's
    device list, PipeWire's registry, WASAPI's endpoint notifications, and
    on the Mac and WASAPI a re-check every 5 s while on the fallback, which
    looks only for another microphone and ignores one that did not
    resolve); its return reads as `DefaultInputChanged`, and the rebuild
    records it again. A fallback the session chose watches for nothing: the
    next rebuild asks for the chosen microphone again. That rebuild comes
    when a default device moves or one in use goes, not when the chosen one
    is plugged in again. A return whose first restart fails ends here too,
    so the recording can stay on the default for the rest of the meeting.
    A Bluetooth headset gone for a second while it changes profile is
    recorded on the default input meanwhile, and stays there when the
    headset does not open at once; each switch costs a short gap, where
    Swift's four failed restarts end the recording.
  - **Settings** lists the stored device as "Microphone not connected" until
    the user picks again or it comes back.
- **Steno's own aggregates are not inputs.** On the Mac,
  `AudioDevices::inputs` leaves out the private aggregates Steno's
  captures create (`uno.schmid.steno.aggregate.*`); Swift lists them as a
  choice while a recording runs.
- **Call mode is its own output client** (fixed by A10 of
  `.plans/2026-10-07-stable-promotion.md`). A tap aggregate runs only while
  a process the tap includes drives the output, so Swift, which excludes
  its own process from the tap, records nothing until another app plays
  (`.plans/spikes/2026-10-01-spike-rust-capture.md`), and keeps that until
  the handoff. Rust includes its own process in the tap and starts a
  silent output IOProc (its input streams set off for it) on the
  aggregate's clock master (the system output, read from the aggregate)
  before the aggregate's IOProc, so with nothing playing the first callback comes within 100 ms
  of `start` returning and 200 ms of the call to `start` (`tests/live.rs`).
  No in-app playback while recording, enforced by `steno_audio::Playback`.
- **The files are synced while recording, the relay holds 20 s, and the
  warnings are joined** (stable plan rows P21 and P23,
  `.plans/2026-10-07-stable-promotion.md`). The writer thread syncs every
  file, the master, the 16 kHz sidecars and the raw microphone when kept
  (`File::sync_data`, `F_FULLFSYNC` on the Mac), after every 500 frames
  written, so a power loss loses about the last 5 s, more while the writer
  is behind, and a recovered recording's sidecars, which its transcript is
  decoded from, are as long as its master. A sidecar costs little beside
  the master: 16 kHz Int16 is about a sixth of a lane's bytes in it. On the Mac every sync, periodic or at the close, falls
  back to a plain `fsync` when the filesystem refuses `F_FULLFSYNC` (a
  WebDAV mount answers ENOTTY), as SQLite does (`writer::durable`). A
  periodic sync that fails even so is logged once, tried again at the next
  interval and handed back at the stop beside the whole recording; a sync
  failure is reported only when nothing else ended the recording (a failed
  write or close, a device loss), and only a failed write cuts the
  recording short. A close whose sync fails is a failure too. The relay
  between the processing thread and the writer holds 2000 frames (20 s)
  instead of 200. A recording that lost half a second or more on its
  worst lane says how many seconds are missing, rounded to the nearest
  second (a lone 10 ms drift slip on Windows does not warn), beside the
  other warnings, which are now joined (a device loss no
  longer hides a silent call), and logs the counts per lane at `warn`.
  Swift synced at the close alone, with a plain `fsync`, its relay held
  2 s and it showed one warning.

What the Windows backend (WP10a, `capture::live::wasapi`) does differently
from the macOS one, each a parity item until a Windows machine has checked
it:

- **A Windows machine is needed** for speech-stack gate G4 (DirectML on an
  integrated GPU) and for a live capture check of the WASAPI backend: the
  `--ignored` tests in `crates/steno-audio/tests/live_windows.rs`, a real
  call recorded through process loopback, a default-device switch while
  recording, and meeting detection with Teams and Zoom. No Windows machine
  has run it. The `windows-latest` CI runner has no audio endpoint, but
  process loopback runs there and delivers silence, so CI checks that a
  system-lane capture starts, delivers whole periods, stops dead and
  restarts; the microphone path never opens there. Under wine with a
  headless PipeWire server the whole backend runs (endpoint loopback, as
  wine has no process loopback), the `--ignored` tests included.
- **Two clocks, no drift compensation.** The microphone and the system audio
  are two WASAPI streams on their endpoints' clocks; the Core Audio
  aggregate drift-compensates, the Windows backend does not. The system lane
  sits in a jitter buffer behind the microphone (`realtime::streams`) sized
  from the system stream's period: a target of two periods (20 ms at the
  usual 10 ms period), a slip back to the target once the lowest queue over
  half a second stayed more than one period above it (counted as dropped
  system frames; a master thread that runs late and drains its packets back
  to back raises the queue only for a moment and slips nothing), an
  immediate slip once the queue is more than the microphone's buffer above
  the high-water mark (no late master explains it), and zeros with a
  re-prime after an underrun. What the follower queued before the master's
  first pull is trimmed to the target, not counted: audio from before the
  recording, or, when the master's first drain is late, the system audio
  recorded during that lateness. The full staging refuses the newest
  packets, so when it refused one before that pull (a microphone that starts
  more than 1.37 s after the system stream), the first pull drops everything
  queued and the refused packets, uncounted too, and the lane primes on the
  audio that follows. `underrun_frames` counts the shortfall and the
  re-prime zeros that follow it, not the zeros before the lane first primes.
  The underrun, slip and trim counts are logged at `info` when the capture
  stops (the shell's default filter is `warn`: set
  `RUST_LOG=steno_audio=info`; `tests/live_windows.rs` prints them). Measure
  the slip rate on a USB headset against built-in speakers; a plan decides
  whether to resample instead.
- **Engine data loss is not a drop.** A packet the engine flags as a
  discontinuity (the capture thread was late and the engine lost data) is
  counted and logged at stop, never added to `CaptureStatistics`' drops:
  WASAPI does not say how much was lost. A Windows recording's drop count
  can under-report where the Mac's does not.
- **A system-only capture skips silence.** With the `[System]` lane
  override the system stream is the master, and endpoint loopback
  delivers no packet while nothing plays, so the recording is shorter than
  the time it ran. Call and in-person captures master on the microphone,
  which delivers continuously.
- **Far-end latency** is the two streams' `GetStreamLatency` less the jitter
  buffer's target. Between slips the queue sits above the target by up to
  one period (10 ms), plus the follower's worst lateness in a window, plus
  up to two windows of drift (a fraction of a millisecond at the drift of
  real clocks), and never more than one period plus the microphone's buffer
  (the immediate slip); that is the echo canceller's alignment error from
  the buffer. The process-loopback client may not implement
  `GetStreamLatency`; a failed read or a latency above 200 ms counts as 0,
  since understating the delay stays inside the canceller's tail and
  overstating it does not. A staging delay larger than both latencies is
  logged at `info`. A loopback stream's latency is not the render path's;
  check the echo canceller's alignment on hardware.
- **Process loopback scope.** Excluding Steno's process tree records every
  other process; whether that follows the default render endpoint or mixes
  every endpoint is unverified. Microsoft's API page names build 20438 for
  process loopback and its ApplicationLoopback sample build 20348; it is
  reported to work from Windows 10 2004, also unverified. Its client is
  reported to answer `GetBufferSize` with 0 or a huge value, so the buffer
  is clamped to between one period and one second, and a device period
  outside 1 to 100 ms is taken for 10 ms (`split_streams::stream_sizes`).
  The fallback, loopback of the default render endpoint (which records
  Steno's own output too), runs whenever process loopback fails for any
  reason, its 5 s activation timeout included. The user gets no notice;
  only the log says which loopback runs.
- **Default roles.** Windows keeps an `eConsole` and an `eCommunications`
  default per direction; the backend follows `eConsole` only. The
  snapshot's `output_uid` is the `eConsole` render default (the endpoint
  loopback's device), `default_output_uid` stays empty, the microphone
  default is the `eConsole` capture default, and `AudioDevices` marks the
  `eConsole` render endpoint as both the default output and the default
  system output (`AudioDevices::default_system_output`, as on the Mac). A
  change of the communications default alone costs no rebuild.
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
  on an endpoint notification, and otherwise caught by the detector's 1 s
  poll.
- **Device list.** `AudioDeviceInfo.id` is the index in the enumeration
  (WASAPI has no numeric ids), `uid` the endpoint id `Settings` stores; the
  transport type and `is_running_somewhere` are not read.
- **A hanging start.** `start` waits at most 10 s in all for both streams
  to open and start and for the watcher to register, holding the session's
  lock meanwhile (the long hold in `capture::session`'s doc). A stream
  thread still in a COM call at the deadline is left running unjoined
  until the call returns. A watcher that missed it but comes alive later
  watches as an on-time one does; `stop()` joins a watcher once it has
  reached its loop, and one still in its start-up calls finds the stop
  flag when they return and exits without reporting.
- **One notification thread per detector start.** Each `start()` of the
  meeting detector calls the session source's `changes()`, which runs a
  thread holding its COM registrations until the source is dropped or,
  after the detector stopped, the next notification arrives. A detector
  started and stopped many times holds that many idle threads until then.

Five Swift defects the port does not share; fix them in Swift if it ships
another release, otherwise the cutover closes them:

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
  monitor, Steno's output included, and only that sink: an app routed to
  another output is not in the lane. Steno plays no audio today (no
  playback in the Rust crates, no `<audio>` in the web UI), so nothing of
  its own is in the lane. Leaving its output out was weighed and not done
  until Steno plays audio while it records (#214); the measurements, on
  the private daemon `scripts/pipewire-headless.sh` starts (PipeWire
  1.6.5, WirePlumber 0.5.14):
  - A playback node names its process only through its client: the node
    carries `client.id`, and that client `application.process.id` (equal
    to `pipewire.sec.pid` for a native client). A property Steno would set
    on its own streams (`steno.own = true`, given to `pw-play -P`) shows on
    the node.
  - WirePlumber converts every stream to the sink's channels: a 5.1
    `pw-play` into the stereo null sink had only `FL` and `FR` output
    ports, each linked to the sink's input of the same channel. Channel
    maps would match; the downmix stays in each stream's adapter.
  - A stream's links go when it ends, and the sink falls from `running` to
    `idle` (later `suspended`) once nothing plays; the monitor link keeps
    it running today.
  - Behind a filter the origin is gone: with a `pw-loopback` whose capture
    side is a virtual sink and whose playback side goes into the default
    sink, the node linked into the sink is the loopback's, in the
    loopback's process, and what it carries is already mixed. An
    equaliser, an echo-cancel sink or a combined sink as the default
    output does the same, so an exclusion at the default sink cannot take
    Steno's audio out of it.

  Doing it would mean one link from each `Stream/Output/Audio` node
  linked into the default sink (Steno's own excepted) to each of the
  capture's system ports, created and destroyed from the PipeWire thread
  as streams come, go and move between sinks (the registry's link globals
  tracked to see where each stream goes; the input ports sum the links);
  those links going must not read as a device gone, unlike Steno's links
  today; and with no monitor link the sink no longer runs with the
  capture, so a stream starting mid-call can move the capture's driver.
  That is a second linking policy beside WirePlumber's, for audio Steno
  does not play, still blind behind a filter. Once Steno plays audio
  while it records, pausing that playback, or sending it to a device
  other than the default, may be simpler.
- **Latencies are the ports' `SPA_PARAM_Latency` lower bounds**: the
  microphone port's capture side plus the sink's first playback port's
  playback side, in frames of the first cycle. Null devices report zero, so
  CI checks the parsing and the arithmetic, not real numbers; `start` logs
  both at `info`. No real hardware has measured them yet: see "Measuring
  the latencies on real hardware" below.
- **`start` waits for the first cycle**, and that is kept: the latencies
  are counted in frames of the cycle (the quantum, 1024 or 2048 frames on
  the private daemon depending on what else runs, and only known once the
  stream runs), a graph that never runs (a driver that does not start)
  fails the start with an error instead of beginning a silent recording,
  and a link the server refuses after creating it fails the start rather
  than the first seconds of the meeting. It fails after `START_TIMEOUT`
  (3 s) without a cycle; a tighter bound would fail Bluetooth sinks, which
  take 1 to 2 s to start. The Mac's returns before any callback, WASAPI's
  once both streams started (at most 10 s). Linking the sink's monitor keeps
  the sink running, so cycles arrive with nothing playing (the Mac's call
  mode keeps its tap aggregate running with a silent output of its own,
  A10). The cost, measured on the private
  daemon over the 24 starts of one run of the live tests at a load
  average of 9: the first cycle comes 2 to 43 ms after the links, and the
  whole `start` (connection, two roundtrips, links, first cycle,
  latencies) takes 6 to 47 ms, 26 ms in the median; over 72 starts at a
  load average of 18 to 27, 5 to 474 ms, 28 ms in the median, and over 114
  at 21 to 43, 18 to 908 ms, 90 ms in the median; `start` logs its
  length at `info`, the first cycle's wait at `debug`. The session holds its
  mutex across `backend.start()`, so its callers, `state()` included, wait
  as long: expected 1 to 2 s for a Bluetooth sink, against the Mac's 200 ms
  at most.
- **Device changes read differently.** A lost connection or stream loses
  both lanes and reads as `OutputDeviceGone` (`InputDeviceGone` in
  person). A lost link (one removed from outside included) reads as the
  device of the lane it serves gone, as the Mac reads each device's own
  liveness: a monitor link as `OutputDeviceGone`, the microphone's link as
  `InputDeviceGone`, so a microphone that vanishes during a call reads as
  `InputDeviceGone`. `SampleRateChanged` never fires: PipeWire's adapter
  resamples whatever the graph runs at. A device destroyed and re-created
  under the same name and id (WirePlumber restarting, a USB device
  re-enumerated) reads as gone, by its `object.serial`; the defaults are
  forgotten while the `default` metadata is gone. Of nodes and ports
  going away, only a node the capture reads (a linked device, the default
  sink, the source the microphone follows) or a port of one is a change,
  so other apps' streams ending do not hold a report back, and a burst of
  changes is judged at most 2 s after its first.
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
- **`stop()` is bounded.** It closes the capture's gate to the sink and
  joins the PipeWire thread, all within 2 s once it has the backend (a
  `start` in progress holds it); a device-change report still inside the
  gate then is logged and left to finish, with the thread it runs on, and
  a thread that has not ended otherwise is logged with the system call it
  waits in and left behind (the devices may stay open until Steno quits).
  Only a cycle's delivery inside the gate is waited for without a bound:
  it takes microseconds, and one still writing the rings once `stop()`
  returned would write them beside the next backend's thread. A report
  runs the session's handler, which may wait for the session mutex; one
  left behind reaches the session late. The session ignores it unless it
  is still recording the recording that backend served, and otherwise
  rebuilds once more: the report starts a rebuild, or becomes the pending
  change of the one in progress. A late report that takes the sink's latch
  after the rebuild re-armed it holds back the rebuilt backend's first
  report, but starts a rebuild itself. So once `stop()` returned no frame
  reaches the sink, and no report but one the gate let in before it
  closed.

  A hang was seen once in testing, most likely in a log write: logs were
  written synchronously then, a capture thread left behind in a later run
  was blocked in `write(2)` to stderr, waiting on the disk's journal at
  idle I/O priority, and `stop()`'s own log of the hang waited for the
  same stderr lock. Since #202 the binaries queue log lines for one writer
  thread and drop a line rather than wait (`steno_services::logs`), so a
  stalled stderr holds neither the capture thread nor `stop()`. Only log
  lines are queued: the shell's `stderr_line!` and the CLI's progress
  lines still write to stderr directly.
- **Device UIDs are `node.name`s.** A Core Audio UID from a synced or
  copied settings file names no Linux node, so it records the default
  source, as any missing chosen microphone does on every platform (see "A
  chosen microphone that is missing or cannot be opened no longer ends a
  recording" in the first Audio list). On the fallback, the capture's
  snapshot follows the default as one without a UID does, and the chosen
  node, or one of its ports, announced again counts as a change
  (`Graph::followed_source`). A virtual source (a null sink with
  `media.class = Audio/Source/Virtual`) records from its monitor output,
  the only output it has.
- **The input device list** (`capture::live::pipewire::AudioDevices`) is
  every source the capture's UID lookup accepts (`Audio/Source` nodes,
  virtual sources, duplex devices), named by `node.description`, else
  `node.nick`, else its `node.name`. One short connection per call on a
  thread of its own, one roundtrip, bounded by `START_TIMEOUT`. It does
  not bind the `default` metadata, so no device is marked the default
  (Settings shows none): a bind dropped before the session manager
  answered its ping, as a list during a WirePlumber stall would leave one,
  stops the metadata's events for every client (see "A default move can go
  unreported"). That includes a capture recording the default source as
  the fallback. A live test lists with WirePlumber stopped and checks a
  capture still hears the next move. The rate reads 0 and
  `is_running_somewhere` is not read.
- **Meeting detection** (`detection::pipewire`, the Linux
  `LiveProcessAudioActivity`) reads the registry from one PipeWire thread
  per source, which starts with the first call and ends when the source
  drops, so every `changes()` receiver shares it (unlike Windows, no thread
  per detector start). A process holds the microphone while one of its
  `Stream/Input/Audio` nodes is linked from a source (a duplex device's
  monitor ports aside) and not idle, suspended or failed: the registry
  reports links with both ends on the global, and a stream recording a
  sink's monitor (a screen recorder) runs too without holding a
  microphone. Left out: Steno's own capture (by its
  `node.name`, so a `steno record` beside the app is no call), a filter's
  or loopback's halves (`node.link-group`: an echo canceller's capture
  stream holds the real microphone for as long as it exists) and level
  meters (`stream.monitor`). The pid is the node's
  `application.process.id`, else its client's, else the client's
  `pipewire.sec.pid`; the `bundle_id` is `application.process.binary`,
  else `application.name`. Native clients report their binary
  (`pw-record` reports `pw-cat`); PulseAudio clients, which most call apps
  and browsers are, report theirs through pipewire-pulse, so a call in a
  browser tab names the browser's binary, not the call service. Swift has
  no list of call apps (the prompt names any holder, by the app name its
  bundle id resolves to), so none needs Linux names (the display name is
  an open item under "First Linux release"). The pid is not always the
  host's: a PulseAudio client's `pipewire.sec.pid` is the pulse server's
  (measured), and a Flatpak app's `application.process.id` is its pid
  inside the sandbox, so two sandboxed apps can share pid 2 and merge into
  one process, and a pid is no handle into `/proc`. A lost connection ends
  the thread; calls within `RETRY_AFTER` (5 s) answer the error, the next
  connects afresh. One dropped while it connects ends at once.

#### Measuring the latencies on real hardware

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

### Handover

Rust fixes the Swift behaviours below except the network, service name, write order,
touch and admission lines; each fix is ported to Swift before cutover.

- Network, same as Swift: Rust refuses tunnels. On Linux and macOS that is every
  point-to-point interface, which most tunnels there are (`wg0`, `tun0`, `utun3`); on
  Windows every adapter but hardware Ethernet and Wi-Fi that is up
  (`advertise::windows_keeps`), which leaves out Wintun, TAP and Hyper-V adapters.
- Network change, both apps: the record follows the computer's addresses. Swift's
  `NWListener` does it in mDNSResponder. Rust's advertiser
  (`crates/steno-handover/src/server/advertise.rs`) registers the record again under
  the same name, TXT record and port. A report from the `mdns-sd` daemon that it
  added an IPv4 address the record carries (its interface check, every 5 s, sent
  after it joined the new network's multicast group) always registers it, because
  only a registration made after the report is announced on a network the daemon has
  just joined. Any other wake registers it only when the LAN addresses moved: a quiet
  recheck a minute after the last wake, whatever else the daemon sends, catches a
  dropped report, a removed address or a change the daemon does not report. A failed
  registration is tried again at the next change, and a record with no address waits
  for the next network. The listener binds every IPv4 address on one port, so an
  address gained after start is served on the same port. Its LAN check runs once per
  connection at accept, so the listener closes no connection; one on an address that
  leaves breaks with it, and the phone resumes from the partial.
- Network change, Rust differs: the record's host is `steno-<name>-<id>.local.`, with
  the first 8 hex digits of the `macID` (`Advertiser::host_name`), a name only Steno
  answers for, where Swift's record uses mDNSResponder's own host; the computer's
  host name stays with mDNSResponder, Avahi or Windows, and two computers of one name
  get two hosts without `mdns-sd`'s probe. A withdraw within one interface check of a
  switch sends its goodbye only on the old network, which is gone, so a phone that
  kept browsing can show the record until its TTL runs out, and its connection fails
  and is retried; a limit of `mdns-sd`.
- Network, Rust differs: layer-2 tunnels (a TAP device, `feth`) and bridges
  (`docker0`, `bridge100`) are not point-to-point, and are served; Swift classes
  bridges `.other`. A LAN numbered in `100.64.0.0/10` is refused, on every platform.
  Rust judges a connection by its local address where Swift judges the interface it
  arrives on, so on Linux and macOS (weak host model) a packet addressed to the LAN
  address that arrives over a tunnel is served: the computer is a subnet router or
  exit node, or a peer's allowed IPs cover the LAN. On Windows the hardware rule
  refuses a LAN address on a Hyper-V external switch's or a Network Bridge's
  vEthernet adapter, so a computer whose LAN address moved there is unreachable.
- Write order: both apps commit every store write of the engine (receipt saves, the
  revoke's delete, the pairing's save, the touch) in the order it was asked for: each
  waits until the one asked for before it has returned, also when the request that
  asked for it is gone (`Engine::in_order`, `HandoverEngine.inOrder`; Swift #205). So a
  revoke during a pairing's save deletes after it and the phone stays revoked after a
  restart, and a pairing during a revoke's delete saves after it and stays. Rust takes
  the place in line under the state lock together with the memory the write stands
  for (the revoke count, the pairing's read of it, the receipt); in Swift the actor
  makes the two one step. Rust makes every receipt change to the copy memory holds
  under that same lock (`Engine::change`), so two chunks that land at once on two
  threads both stay, and so does a chunk that lands during a re-announce or during
  another first announce of the same recording. In both apps a receipt memory holds
  as `complete` stays `complete`, and a late write of a revoked device leaves another
  device's receipt alone (`Engine::update`, `HandoverEngine.transition`; Swift #209).
  In both, a first announce whose store read found nothing answers as a re-announce
  when memory holds the receipt by then, so the chunks folded into it and a `complete`
  stay (Swift's `RecordingHandler.readReceipt`). Rust opens a new recording's files
  under the same hold of the files lock as the `change` that made its receipt
  (`Engine::make_and_open`, `Engine::open_files`), so of two first announces that
  race, the sidecar is the metadata of the one whose receipt memory holds, and the
  late one leaves it alone; a failed opening still saves the receipt at its place in
  line, and the phone's retried announce opens the files as a re-announce. A first
  announce whose device was revoked since its receipt read also saves the receipt at
  its place in line, opens no files and answers 401. Swift checks memory, discards the
  recording id's files, runs `begin` and makes the receipt in one step on the actor.
  The intake's own receipt saves (`RecordingIntake::admit`, `RecordingIntake.admit`)
  run outside the line in both apps, so the intake itself checks that it completes
  only the admitting upload's receipt (the Store item on `RecordingIntake.admit`). A
  line save asked for before the intake's commit (a chunk or a re-announce during
  `complete`) can still land after it and put `verifying` or `receiving` back over
  `complete`; the engine's own `complete` save after the intake sets it right, and if
  that save fails or the app stops first, the stored receipt still reads as
  admitted, because the admission ledger holds its bytes (the "Admission" item), and
  the phone's retry gets the first meeting.
- Touch: both apps (Swift #205) run an `UPDATE` of the row that still holds the token
  (`Store::touch_paired_device`, `MeetingStore.touchPairedDevice`), so a revoke that
  commits between the gate's read and its touch stands.
- Announce, the same in both apps (`.plans/2026-10-08-handover-admission-ledger.md`,
  #243): `Engine::announce` and `HandoverEngine.announce` read the receipt, and the
  admission ledger only where it decides something (no receipt, or one of other
  bytes). Bytes the ledger holds are answered 200 `complete` with every chunk
  listed, from a `complete` receipt saved for the announcing device with the
  ledger's meeting id: after a revoke deleted the receipt, after the user deleted
  the meeting, or over a receipt of other bytes that is `complete` or the announcing
  device's. Over another device's unfinished upload of other bytes they are 409
  "another device owns this recording": answering `complete` there could delete that
  phone's copy. Other bytes the ledger does not hold are a new recording under the
  same recording id: a fresh receipt and empty partial files, 201, and a meeting of
  its own once its `complete` commits; the ledger then holds both admissions. The
  same bytes from another device take the receipt over with its chunks and files
  (`Engine::take_over`; Swift sets the device and remembers the receipt in the same
  actor step): the hash proves the same recording, so no check that the new device
  replaced the old one is needed, and the older device's next announce takes a
  `complete` receipt back and is answered delivered. The same bytes in another
  split restart the partial under it before `complete` (200, no chunk listed); a
  `complete` receipt announced in other chunks answers 200 with every chunk of the
  announced split, and keeps its own split, which `GET status` lists, so a phone
  that reads the status before its `complete` uploads the chunks of its split
  missing there again; each PUT on a `complete` receipt answers 204, which wastes
  uploads and loses nothing. Format, duration and `startedAt` are not compared:
  under the same size and SHA-256 they describe the same bytes. A replacement
  (Rust's `Engine::replace` through `Engine::make_and_open`, Swift's `replace`)
  declines when memory holds another receipt than the one decided on by then (a
  first announce or a chunk on another thread, or during the ledger read) and the
  announce decides again with that one. Every receipt write of a request is dropped
  when memory holds another upload by then (`same_upload`,
  `HandoverEngine.sameUpload`: another device, or other bytes; in `Engine::update`,
  `Engine::add_chunk`, `HandoverEngine.transition` and the chunk fold), so a late
  `complete` of replaced bytes never marks the new upload `complete` with their
  meeting; a chunk, or any other write of a chunk set, is also dropped over a
  receipt in another split, so a late `complete` of the earlier split leaves the
  new split's chunks. When the intake refuses such a `complete`, its verified file
  goes, so the new upload's `complete` cannot admit it unhashed. A takeover while
  the older device's `complete` is in the intake can put the newer device's
  unfinished receipt back after that admission committed; the newer device's upload
  then reaches an admission whose transaction finds the bytes in the ledger and
  completes the receipt with the first meeting (the Store item on
  `RecordingIntake.admit`), so no second meeting is made. One gap stays in both
  apps: a late request of a replaced upload can still discard the new upload's
  files (the phone announces again and sends its chunks again).
- Store reads: Swift's `RecordingHandler.receipt` reads with `try?`, so a failed read,
  with no receipt in memory (after a restart), counts as no receipt: status, chunk and
  complete answer 404. `HandoverEngine.authenticate` reads the device with `try?`, so
  a failed read answers 401 and the phone unpairs. Rust answers 500 (`Engine::receipt`,
  the bearer gate). Swift's announce now does the same: it reads through
  `RecordingHandler.readReceipt`, which throws, and answers 500 with nothing opened, so
  it never starts the recording over on a failed read. A failed read while memory
  holds the receipt by then still answers with it in Swift (`readReceipt`), where
  Rust's `Engine::receipt` answers 500; memory's copy is the newer one, and the phone
  retries a 500, so neither loses anything. The sweep keeps the files of a receipt it
  cannot read in both apps (`Engine::sweep_orphans`, `HandoverEngine.sweepOrphans`;
  Swift #213).
- Admission, the same in both apps (#213): a stored `complete` receipt whose meeting
  row is missing reads as `failed` ("the admitted meeting is missing"), not admitted
  (`stored_receipt` in `crates/steno-handover/src/engine/mod.rs`,
  `HandoverEngine.storedReceipt`), wherever the engine reads a receipt from the store:
  `complete` and the re-announce (`Engine::receipt`, `RecordingHandler.readReceipt`) and
  the start's sweep. Only the separate receipt and meeting commits of earlier
  releases leave one (a crash or a full disk between them; the Store item on
  `RecordingIntake.admit`): its phone never got the 200 and holds the recording, so
  the sweep keeps the verified file, a re-announce lists the chunks without saying
  `complete`, and the phone's `complete` admits the file again. Since schema v5 the
  admission ledger decides first: a stored receipt whose recording id, size and
  SHA-256 it holds reads as `complete` with the ledger's meeting id, whatever its
  state, so a `complete` receipt whose meeting the user deleted (deleting a meeting
  deletes its receipt, `Store::delete_meeting`, `MeetingStore.delete(meetingID:)`,
  but a later announce saves one again from the ledger) and one a late line save put
  back to `verifying` both read as admitted. A receipt in memory is trusted as it
  is.
- Pairing windows: Swift's `HandoverEngine.pair` checks only that a window is open,
  not that it is the one whose secret the head matched. The read timeout runs per
  silence, so a head whose body keeps trickling in pairs against a window opened after
  a cancel, or one opened for a second phone. A failed save gives the session back
  whenever no window is open, also after a cancel. Rust numbers the windows and pairs
  only against the one the gate matched (`Principal::Pairing`); a failed save does not
  reopen a window cancelled or replaced meanwhile.
- Revoke during a `complete`: `revoke` discards the files of the receipts in memory
  only, so after a restart a revoke during `complete`'s receipt read let the revoked
  phone's file reach the intake, also when the phone paired again meanwhile. Both apps
  (Swift #191) count revokes per device (`HandoverEngine.revocations`,
  `State::revocations`); pairing never resets the count. `complete` takes it before
  the read and refuses when it moved: right after the read (401, files discarded,
  receipt forgotten) and after the hash (see the differences below). A `complete` that
  starts while a revoke is in flight is refused at entry: Swift's store is a WAL pool,
  so a read can see a row whose delete has not committed; Rust's is one connection
  behind a mutex, but the blocking pool may serve a read queued after the revoke's
  bump before the delete. `pair` clears `revoked` only when no revoke started during
  its save (Swift `revokeStarts`, Rust the count). A recording already admitted
  answers 200 with its meeting id after a revoke during the read, and its receipt
  leaves memory. The phone marks that recording `delivered` and deletes its copy also
  when an unpair moved the row to `unpaired`, or a new pairing queued it again, while
  the `complete` was out (`TRANSITIONS` in `mobile/src/features/queue/queue-index.ts`
  allows `unpaired` and `queued` to `delivered`), so after a `complete` whose 200
  reaches the phone no pairing uploads it again as a second meeting. Until that
  `complete` answers, the planner (`planNext` in
  `mobile/src/features/sync/upload-coordinator.ts`) announces nothing for the row, so
  its 200 cannot delete the file under a new pairing's upload. A 401 to that
  `complete` leaves the row `unpaired` with its file, for the next pairing to upload,
  or `queued` with a backoff after a new pairing, which uploads it. Both apps bind the
  hash to the file the intake gets: the partial's identity is taken before the
  `verifying` write and checked before the promote, and a partial gone or created
  again meanwhile (a stale `complete`'s refusal, then the phone's retried announce)
  answers 409 with no chunk listed, so the phone sends every chunk again instead of
  the intake admitting an empty file. Swift compares APFS file
  numbers, which are never reused. Rust holds the partial open until the promote, so
  its number cannot go to another file. Both apps share two gaps. The files of a
  revoked device's receipt that is only in the store, and not being completed, wait
  for the next start's sweep. A phone that pairs again and announces anew while an old
  `complete` waits right after its store read has its new files discarded by that
  `complete`'s refusal; the phone uploads again, nothing is lost. The differences:
  Rust refuses every recording route (announce, status, chunk, complete; not unpair)
  from the revoke until the device pairs again; Swift refuses `complete` only while
  the revoke runs, after which the read answers 404. After the hash Swift discards the
  files whatever the verify answered, unless memory holds another device's receipt
  ("Files by recording id" below); Rust answers 401 without a write and discards a
  partial there only while `revoked` still holds the device (the revoke discarded the
  files, so only the revoked phone can have created it); once the phone paired again
  it is the new pairing's upload and stays. On a failed store delete both republish
  the receipts. Swift takes back the count when nothing was discarded and `revoked`
  unless another revoke is in flight; Rust takes back neither, so the device stays
  paired in the store but its recording routes answer 401 until it pairs again, a
  retried revoke finishes or the app restarts: a half-revoked phone that cannot hand
  over is safer than one that can.
- Files by recording id: Rust keeps a recording's inbox files to one device under two
  rules. Nothing goes, and no receipt is forgotten, while memory holds another
  device's receipt of that recording id (`Engine::discard_own`,
  `Engine::discard_own_while_revoked`, `Engine::discard_and_forget_own`,
  `Engine::forget_own`). And files are created only for a device that `revoked` does
  not hold: by a first announce (one whose read found no receipt in memory or the
  store) while memory holds the receipt its `change` made, or by a re-announce of the
  receipt's owner; a first announce or re-announce whose device was revoked since its
  receipt read opens none and answers 401 (`Engine::open_files`,
  `Engine::reopen_missing_files`). The check and the discard, like the creation, hold
  the engine's files lock (taken before the state lock, held across synchronous calls
  only), so no announce opens files between the two; a first announce holds it from
  its `change` through its opening. Two stretches with no yield in them, on other
  threads, still leave files whose receipt memory does not hold at the check: a
  pairing again after a re-announce's read and before its opening (the re-announce's
  write puts the receipt in memory right after); and a second revoke between
  `Engine::reopen_missing_files` and the re-announce's write (the reopened partial and
  sidecar belong to no receipt in memory or the store until the next start's sweep or
  a first announce of that id). A first announce discards every file of the recording
  id before `begin`, in `Engine::open_files`, within the hold of the files lock that
  made its receipt; so does an announce that replaces the receipt it read (the
  "Announce" item), whose files are the replaced upload's. Those files belong to no receipt: a verified file an intake
  failure left whose receipt a revoke deleted after a restart (the revoke found no
  receipt in memory, and its delete cascades the row), or a partial a refusal or a
  second revoke left. Kept, `complete` would admit the old verified file unhashed (the
  early return of `verified_file`), or `begin` would add the new chunks to the old
  partial. No live upload of another device can be in them: every recording route
  reads its device's receipt into memory before it touches a file, memory drops it
  only when that device is revoked, and `change` made the new receipt only because
  memory held none. So only a revoked device's request can still be at work on those
  files. Such a request answers a refusal, an error or a re-announce's status, and the
  phone deletes its copy only on a 200 from `complete`, so it keeps its recording; or
  it answers the 200 of an admission whose intake opened the verified file before the
  discard and copies it whole. A first announce whose device was revoked since its
  receipt read opens nothing, also when the device paired again and a request wrote
  another device's receipt back meanwhile, so it never writes its sidecar over that
  device's. Every discard but the start's sweep follows the rules, the first
  announce's through its own check in `Engine::open_files`: the failed save of a first
  announce (`announce` in `crates/steno-handover/src/engine/recording.rs`), the
  admission, a `complete` refused for a revoke (after the read, which also forgets the
  receipt, and after the verify), the revoke, the 422 and a re-announce that finds the
  receipt `complete`; so does the forget of a recording admitted before, whose
  `complete` answers 200 after a revoke. So when a phone is revoked while its
  recording is verified or in the intake and another phone announces the same
  recording id, that phone keeps its receipt, partial and sidecar, and its upload goes
  on. On success the admission removes every file of the recording, also the partial
  and sidecar a re-announce opened during the intake, else an empty partial would wait
  for the next start's sweep. Swift keeps the first rule: nothing goes, and no receipt
  is forgotten, while memory holds another device's receipt of that recording id
  (`RecordingHandler.ownedByAnotherDevice`), and the actor makes each check and its
  discard one step, with no `await` between them, so no announce lands between the
  two. The rule covers the admission, a `complete` refused for a revoke (`refusal`,
  after the read and after the verify, both of which also forget the receipt) and the
  discard after a first announce's failed receipt save (`announce` in
  `Sources/StenoHandover/Routing/RecordingHandler.swift`), which runs after the save's
  suspension, so a revoke and another phone's announce can land before it. After the
  intake `admit` removes the verified file it handed over whatever memory holds (no
  other request creates it while the `complete` holds its `completing` mark), then the
  rest of the recording's files under the rule. Four discards need no check: the
  revoke removes the files of its own device's receipts in memory; the 422 after the
  hash follows the revocation check in the same actor step, so memory holds the
  phone's own receipt or none; nothing suspends between the receipt read and the
  forget of a recording admitted before, whose `complete` answers 200 after a revoke;
  and a first announce discards every file of the recording id before `begin`, in the
  same actor step as the read's last look at memory and the receipt's `remember` in
  `persist`, so no request lands in between. As in Rust, those files belong to no
  receipt (the verified file of an intake failure whose receipt a revoke deleted after
  a restart, the files of a revoked phone's re-announce below), and no live upload of
  another device can be in them; a failed store read answers that announce 500
  (`readReceipt`), so a receipt only in the store keeps its files. A re-announce that
  finds the receipt `complete` needs no discard: it reads memory and calls `begin` in
  one step, and `readReceipt` checks memory again after its store read, so no file is
  opened once memory holds the `complete` receipt. Swift has no second rule:
  `announce` does not check `revoked`. A revoked phone's first announce before the
  revoke's delete commits (the phone still passes the gate) remembers nothing, its
  save queues behind the delete and fails on the foreign key, and the discard after
  the failed save removes its files under the rule. Its re-announce in that window
  finds the store row and opens files that belong to no receipt in memory (`remember`
  skips that device); its save fails the same way, with no discard, so the files wait
  for the next start's sweep or another phone's first announce of that id, which
  discards them.
- Service name: Swift's `HandoverConfiguration.defaultServiceName` uses
  `Host.current().localizedName` (the computer name in System Settings), else
  `ProcessInfo.processInfo.hostName`. Rust's `HandoverConfiguration::default_service_name`
  takes the first of `NAME_SOURCES`: on the Mac the computer name
  (`whoami::devicename`, which reads the same `SCDynamicStoreCopyComputerName` as
  Swift), so a phone shows the same name after the handoff; then, and on Linux and
  Windows first, `HOSTNAME`, `/etc/hostname` and the system's host name
  (`whoami::hostname`). Rust differs: its last fallback is `Steno` when none gives
  one, where Swift's is the host name. The name is shown, never pinned: the phone
  finds the computer by the TXT record's `id`, resolves the instance it browsed, and
  pins the certificate's fingerprint; the identity's `CN` takes the name only when
  an identity is minted. A phone keeps the name it paired under (`Steno` from an
  earlier Rust build) until it pairs again. While the Swift and the Rust app run at
  once during the handoff, both claim the same instance name and one is renamed
  "(2)"; the name and the rename are display only, since the phone finds the
  computer by its `id`.
- Phone queue (`mobile/src/features/`):
  - Adoption. Every load lists `Documents/queue/` and adds a row for each recording
    file of 1 KiB or more (`MIN_RECORDING_BYTES`; smaller holds no meaningful audio)
    that no row names (`adoptRecordingFiles`), so a corrupt index with no usable temp
    file, a stale temp file or a row that was never saved leaves no recording behind.
    A corrupt index, including one whose bytes are not text, is kept as
    `index.corrupt-<ms>.json`; a temp file that cannot be read is kept as
    `index.unreadable-<ms>.json`. The move never replaces a file, so no copy is lost
    to a later one. Neither a failure to set them aside nor an unreadable temp file
    fails the load. A save whose rename fails keeps the temp file, since expo's
    rename removes `index.json` first, and the next load reads it.
  - Unhashed rows. Before adopting, the load settles each row that was never hashed
    and is not `recording` (`settleUnhashedRows`). One whose file holds audio, in the
    queue or as its own `sourceUri` in `Documents/ExpoAudio/`, goes back to
    `recording`; a `failed` one without stays failed, and a `queued` or `unpaired`
    one without (a Retry of an earlier version, then perhaps an unpair) fails as
    interrupted, with no Retry. A recorder file several rows name is no row's own (an
    earlier version wrote every recording of one run to one file), so it goes only to
    a `recording` row. Crash recovery hashes and queues these rows, and fails one with
    no file of 1 KiB or more. The upload planner skips a row with no hash, and Retry
    is offered only for a failed row with one.
  - Recorder files. Each `recording-<UUID>.m4a` of 1 KiB or more in
    `Documents/ExpoAudio/` that no `recording` row names, left by a crash before its
    row was saved, moves into the queue as `<uuid>.m4a` without replacing a file
    (`recorderFilesToMove`).
  - Crash recovery (`recorder/recovery.ts` and `recorder/recovery-files.ts`). It
    finds a row's recorder file by the file name of its `sourceUri` in the current
    `Documents/ExpoAudio/`, never by the stored absolute path, since iOS moves the
    app's container to a new path on an update or a restore. It replaces only a
    queue file read as below 1 KiB, and when it refuses, it reads the queue file
    again and goes by that size. A queue file whose size cannot be read (expo reads
    it as null on iOS) counts as present: recovery leaves its row in `recording`
    with a note that it tries again at the next launch, and a later launch does.
  - One file per recording. The recorder prepares expo-audio with the recording
    preset (`use-recorder.ts`), which builds a new recorder at a fresh
    `recording-<UUID>.m4a`, so a failed recording's file is not overwritten by the
    next one in the same app run.
  - A failed load. No load runs while the recorder writes: recording starts only once
    the queue is loaded, and loads run, at launch and when the app comes to the
    foreground, only until one succeeds. A load that fails saves nothing
    (`queue-store.ts`): the index stays as it is on disk, every update first loads
    again and rejects while that fails, and the recorder screen says the list could
    not be read, with Try again; pairing and unpairing wait for the load.
  - Re-uploads. A file whose row is `delivered` keeps that row. A file left by a
    `delivered` row the lost index named is uploaded again: the Mac answers the
    announce from its `complete` receipt, or from the admission ledger once a
    revoke or a meeting delete took the receipt, with every chunk listed, and
    answers the phone's `complete` with the meeting id, so no second meeting is
    made.
  - 409. Three announces in a row answered 409 mark the row `failed` with a message
    and Retry, instead of retrying forever (`ANNOUNCE_CONFLICTS_BEFORE_FAILED` in
    `mobile/src/features/sync/upload-executor.ts`). Since schema v5 the Mac answers
    an announce 409 only for admitted bytes over another device's unfinished upload
    of other bytes: it takes a receipt of the same bytes over and other bytes in as
    a new recording (the "Announce" item).
- Pairing QR code, both apps: the pairing URL (`PairingPayload::url_string`) as a
  QR code at error correction level M, black on white, 8 pixels a module. Swift draws
  it with Core Image's `CIQRCodeGenerator` as an RGBA PNG; Rust with the `qrcode`
  crate as an 8-bit greyscale PNG with a one-module margin
  (`steno_services::qr::PngQrEncoder`), and a test reads it back with `rqrr` to the
  exact URL, which `PairingPayload::parse` turns into the payload it came from. The
  phone scans the text (`PairingSheet.tsx`, `parsePairingPayload`), so the image
  format makes no difference to it. Both answer no image for a text too long for a
  code.

### Shell

- Launch at login is a Launch Agent through `tauri-plugin-autostart`, where the Swift
  app registers with `SMAppService`; the `requiresApproval` state never occurs on the
  Rust side. On macOS the cutover moves the Rust app onto `SMAppService.mainApp` too,
  so the approval copy stays reachable; with the new identifier the app registers
  itself, and the Swift entry is handled as `.plans/2026-10-07-stable-promotion.md`
  (D4, S6) says.
- Packaged installs (stable plan X5, Rust only): `STENO_DISTRIBUTION=aur` or `=nix`
  (the environment first, then the value the build was given;
  `steno_services::updates::updates_are_managed`) turns the update check off, and
  Settings > General says "Updates come from your package manager." in its place
  (`HostConfig::updates_managed`, the contract's `updates.managedNote`).
  `STENO_LOGIN_ITEM=managed`, which the NixOS module sets, makes the login item
  `LoginItemStatus::Managed`: the app never writes, rewrites or removes the autostart
  entry, the first launch registers nothing, Settings shows the switch on and locked
  (`loginItemNote`). The one entry the app removes is an earlier build's, whose `Exec`
  starts a program in `/nix/store` or names one of the profile paths: at launch, or,
  while the app runs as the unit systemd made from that entry, at the exit after the
  save, so a reload of the user manager cannot leave the recorder in a unit no logout
  stops. Otherwise the Linux entry names a stable path (`$APPIMAGE`,
  `STENO_EXEC_PATH`, or the first of `/usr/bin`, `/usr/local/bin` and the Nix profiles
  that resolves into the running binary's directory), never `current_exe()`; with none,
  turning Launch at login on fails with "Steno can't open at login from where it's
  installed now. Restart Steno, or install it with your package manager." and the
  setting is not saved
  (`apps/desktop/src-tauri/src/packaged.rs`, `apps/desktop/README.md` "Packaged
  installs").
- The menu bar on macOS carries the application, Edit and Window menus; the Swift
  Record menu (`⌘⇧R`, Record In Person) and Find Meetings (`⌘F`) are not in it yet.
  The page answers the shortcuts it shows itself (`useShortcut` in
  `apps/macos/web/src/lib/platform.tsx`): ⌘F and ⇧⌘E in both apps, ⌘⇧R in the
  Tauri shell only, where no Record menu owns it.
- Each platform's words and keys: the shell tells every page its platform before the
  page runs (`apps/desktop/src-tauri/src/platform.rs`, `window.__STENO_PLATFORM__`),
  the page words itself from one table (`apps/macos/web/src/lib/platform.tsx`: "this
  computer", "Show in File Explorer", "Show in folder", Ctrl+F) and the host from
  `HostConfig::platform` ("this computer" in its sentences, and only the permissions
  the OS has: all four on the Mac, the microphone and local network on Windows, the
  microphone on Linux). The tray says "Settings" and "Check for Updates" without the
  ellipsis off the Mac and "Exit Steno" on Windows, and shows shortcut hints on the
  Mac only. A call's folder note says "Windows call" or "Linux call" in its info line
  off the Mac (`RenderOptions::platform`, from `Platform::CURRENT`); the frontmatter's
  `source` stays `mac-call`. Only the Mac's pages leave the traffic lights their
  inset (`titleBarInset`); under the native title bar of Windows and Linux the
  sidebars open without the spacer and onboarding at a 24 px top. The Swift app sets
  no platform and keeps the Mac's words.
  Three changes reach both apps: a vault the CLI named reads "Obsidian (<vault
  folder>)" in the footer, onboarding page 1's button says Continue, and the retention
  sentence says "Recordings are kept until you delete them" and points at Settings >
  Recording. The page now answers ⌘F and ⇧⌘E in the Swift app too, where their hints
  showed but nothing answered.
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
- One process per database (Rust only; the stable plan's P15). The app takes an
  exclusive advisory lock on the database's own lock file, beside it with the extension
  `lock` (`<support>/steno.lock` for the default `steno.sqlite`), before it opens it,
  and holds it until it exits (`steno_core::DatabaseLock`, `flock` or `LockFileEx`,
  which the OS drops when the process ends however it ends). An update's relaunch starts
  the new process before the old one exits, so the app waits up to 5 s for a held lock
  (`AppOptions::lock_patience`); the CLI does not wait. A lock file this user cannot
  write (left by `sudo steno`) is locked through a read-only handle; on a filesystem
  without locks the app runs without one and logs a warning, and some network
  filesystems accept the lock without enforcing it (WebDAV on the Mac, measured). A
  second app on the same database says "Steno is already running" and ends before it
  opens a window (`refuse_to_start` in the shell's `main.rs`, which also ends the
  process after 60 s should the alert never close): an app under the old identifier
  beside one under the new, a Linux session without a bus (no single-instance guard
  there), or a single-instance connect that failed. The CLI's commands that write
  (`process`, `deliver`, `dev db`) refuse while another process holds the lock. The ones
  that only read (`export`, and the settings reads of `dev models`, `dev llm` and
  `bakeoff --cleanup`) take the lock when it is free, open the database as usual,
  migrating included, and let go of the lock before they read, so a long read never
  keeps the app out; beside the app they open it without migrating
  (`Store::open_without_migrating`), so a newer CLI never changes the schema under an
  older app, and refuse while the database lacks a migration the CLI would apply.
  Without the lock each instance failed the other's live recording at launch, processed
  the same meetings and ran its own retention sweep. The Swift app and the Swift `steno`
  CLI take no lock, and the Swift app ships no further release, so on the Mac the Rust
  app looks for it by bundle id (`NSRunningApplication`) and refuses to start while it
  runs ("An older Steno is running"). A Swift app started after the Rust app is still
  not kept out, and its launch can fail the Rust app's live recording;
  `fix/recording-recovery` (#233) makes a Rust launch leave a `recording` row alone
  while its master still grows.
- Updates: Sparkle checks daily on its own (`SUEnableAutomaticChecks`,
  `SUScheduledCheckInterval` 86400 in `apps/macos/project.yml`). The Rust app's update
  schedule (`steno_services::updates`, S4) checks at launch and every hour when the
  last check is missing, a day old or in the future. Its flags are the booleans
  `steno.updates.automaticChecks` and `steno.updates.automaticDownload` in
  `preferences.json`, where the Mac's first launch copies Sparkle's
  `SUEnableAutomaticChecks` and `SUAutomaticallyUpdate` (S6); a missing key reads as a
  fresh Sparkle install, checks on and downloads off. The last check time is
  `lastCheckAt` (RFC 3339 UTC) in `update-check.json` in the support directory, a file
  of its own written with `replace_file` after a check that succeeded (Swift:
  `SULastCheckTime`, which the import leaves behind); a failed check is tried again at
  the next hourly tick, and a file that does not parse reads as no check yet. The
  tray's and Settings' check records through the same schedule, and a check that has
  not answered after 60 seconds fails, so a stalled request cannot hold the next one.
  Where Sparkle showed its alert for a found update, the schedule raises the shell's
  "Install and Relaunch" dialog once per version in a run, and not while a recording
  starts, runs or stops: the first tick after it ends raises it. A yes given once a
  recording has started since the dialog came up asks again ("Installing stops and
  saves the recording in progress." with "Not Now", the default, or "Install and
  Relaunch"; the confirm comes up where the alert's Install was, so a second Return
  would otherwise pass it unread), and "Not Now" leaves the version to be raised again
  after the recording. The install downloads first and then holds recording starts off
  (`Recorder::hold_starts`) from just before the updater writes the package through
  the relaunch: a Record meanwhile, from the sidebar or the tray, is refused and says
  "Steno is installing an update. You can record again once it relaunches, or if you
  cancel the install."; a meeting prompt's Record, once detection raises one, goes
  through the same `recording.start` and is refused the same way. The hold also spans
  the updater's password prompt: a `.deb` install always asks (pkexec, then a zenity
  or kdialog password dialog, then a terminal `sudo`), and on macOS an app folder the
  user cannot write asks for an administrator. Cancelling every prompt fails the
  install, which drops the hold and clears the message. A recording the user did not
  agree to stop, such as one that started during the download, puts the install off:
  the package is kept, the version is raised again at the first idle tick, and the
  next yes installs without a second download. A confirm's yes may stop only the
  recording it named; one still starting when the confirm came up is named by the
  meeting id it has at the yes.
  Automatic downloads wait for P25's `InstallGate` (stable plan): its stand-in
  `NeverIdle` never gives a hold, so until P25 lands the flag downloads nothing and
  every install waits for the user's yes; with the gate the schedule downloads by
  itself only while `InstallGate::is_idle_now` says idle (a recording or a processing
  job has the disk and the network to itself), keeps the package until an install
  takes it, and installs only with the gate's hold from `try_hold`, beside the
  recorder's start hold. Sparkle installed a download at quit. Settings' footer
  ("installed when you relaunch Steno", `general-section.tsx`) describes Sparkle; in
  the Rust app the dialog's yes installs once downloaded, after a second confirm when
  a recording has started since the dialog came up, and the shared copy follows when
  the Swift app retires (`.plans/2026-10-04-mac-cutover.md`). One install runs at a
  time, and the install is of the version the dialog named; a yes for a version a
  later check replaced installs nothing, since the newer one has its own dialog. The
  schedule's launch tick waits for the keyring's answer where the Secret Service asks,
  so its dialog does not come up beside that prompt. A packaged install
  (`STENO_DISTRIBUTION=aur|nix`, the environment before the build's value; stable
  plan X5, `updates_are_managed`) runs no schedule, and its checks, the tray's
  included, fail without a request; the tray says the package manager delivers the
  updates. A smoke run (`STENO_SMOKE_SECONDS`) and the fixture host pass no update
  source, so they never check on their own; the tray's check there has the same 60
  second limit and only says what it found. The network is the updater's: the same
  lane manifests, nothing new sent.

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

## Open after the port

What the merged packages left open. Each item starts with its owner: **WP9b** (the Mac
cutover, `.plans/2026-10-04-mac-cutover.md`; which items block the stable release, and the
package that closes each, are in `.plans/2026-10-07-stable-promotion.md`), **First Linux
release**, **First Windows release** (WP10's hardware checks and the unsigned installers),
the branch of a follow-up pull request, or **Unowned** (no package or release has it yet).
Then it says what is open, where it lives (pointing to this plan where the plan already
covers it) and which pull requests found it. The pull request that fixes an item deletes it.
Whatever its owner, an item that can lose a recording, a transcript, a note or a pairing
blocks the stable release (`.plans/2026-10-07-stable-promotion.md`, D3). An item that a row of that
plan's tables names belongs to that row's package (S, A, P or X), whatever its label here.

- **WP9b.** The unticked lines of the parity list; which of them block the stable
  release, and which follow it, is the blocking list of
  `.plans/2026-10-07-stable-promotion.md` (D3), which replaces the rule that all must
  be ticked before the cutover opens. The lines: the menu bar's queue and recent
  meetings, the detection prompt, the auto-stop after a call, the calendar lookup, the permissions probe, the macOS menu
  bar's Record and Find Meetings items, and the "Where the speech sidecar runs
  Parakeet v3" line under "Speech" (its two items below). Several name WP5 or WP8,
  which merged without them. The other unticked "Speech" line, `SpeechSettings`,
  covers Rust-only settings with no Swift behaviour to match: it does not gate the
  cutover and has its own **Unowned.** item. Where:
  the unticked lines under "Beyond the bridge" and "Speech" and the open items under
  "Pipeline and services (WP6b)" and "Shell" in the parity list. Found: #170, #172,
  #173.
- **WP9b.** The shell's two gaps, both following the stable release: no tray badge
  for pending speaker reviews, and a fake clip player (a speaker's sample does not
  play). Where: the fake in `build` (`crates/steno-services/src/app.rs`), the tray
  (`apps/desktop/src-tauri/src/tray.rs`); the WP9 paragraph and seam (4) under
  "Pipeline and services (WP6b)". Found: #173, #185.
- **WP9b.** The Rust app cannot download the Mac's CoreML Parakeet model: Settings
  answers "This build cannot download the CoreML Parakeet v3 model", so only a Mac
  where the Swift app installed it can transcribe, and a fresh install of the cutover
  build (cutover test 5) has no speech model. Where: `ModelStoreSpeechModels::download`
  in `crates/steno-services/src/speech.rs`. Found: #173.
- **WP9b.** Whisper, Ultra and DE cannot be installed on the Rust side: a stored
  `whisperkit-large-v3-turbo`, `parakeet-ultra` or `parakeet-de` runs Parakeet v3 in
  the speech sidecar on every platform, the Mac included, while its Settings row
  answers "has no Rust engine yet", so only processing installs the export, unseen.
  Swift users who chose one of them carry it across the cutover. Where:
  `speech_asset` and `download` in `crates/steno-services/src/speech.rs`; the "Where
  the speech sidecar runs Parakeet v3" item under "Speech" in the parity list. Found:
  #189.
- **WP9b.** Processing a meeting before the speech models are installed starts a
  silent 2.6 GB download inside the pipeline wherever the sidecar speech engine
  (`SidecarSpeechEngine`) runs, on the Mac for Whisper, Ultra and DE: it installs its
  models on first use, and the Settings row shows no progress. It blocks the first
  Linux release too. Where: the "Where the speech sidecar runs Parakeet v3" item under
  "Speech" in the parity list. Found: #189.
- **WP9b.** Settings still describes the diarizer as the Swift app's CoreML model (its
  acknowledgement and its size), while every platform, the Mac included, runs the ONNX
  pyannote segmentation and WeSpeaker embedding models, whose licence notices the app
  does not show yet. It blocks the first Linux release too. Where: `display_name`,
  `source_repo` and `expected_bytes` in `crates/steno-services/src/speech.rs`; the
  Settings > General item under "Pipeline and services (WP6b)". Found: #164, #183.
- **WP9b.** The other Swift fixes and cutover decisions in the parity notes: the
  Swift defects (each ported to Swift if it ships another release, otherwise closed by
  the cutover), the fixtures the Swift side owes, and the audio choices to settle at
  cutover (the WAV mixdown, the resampler, the sidecar's 2 ms lag).
  Where: "Store", "Adapters", "Handover", "LLM",
  "Audio" and "Bridge" in the parity list, and the CLI's `--title` under "Pipeline
  and services (WP6b)". `.plans/2026-10-07-stable-promotion.md` (D9) settles all of
  them, the parity notes' other "before cutover" ports to Swift included; its S7
  deletes this item. Call mode without an output client is fixed by A10.
  Found: #155, #165, #166, #167, #169, #190.
- **WP9b.** No concurrency group spans the two release workflows, so two macOS signing
  jobs can run at once; only both READMEs state the one-at-a-time rule, until
  `release.yml` retires at the cutover. Where: `.github/workflows/release.yml`,
  `.github/workflows/desktop-release.yml`. Found: #184.
- **First Linux release.** No real GNOME or KDE Plasma session has logged out or shut
  down under the app: the session clients ran against fakes on a private bus, the lost
  display under Xvfb and headless sway, and real logouts only in xfce4-session 4.20.4
  (X11, and Wayland under labwc); GNOME's logout dialog for the inhibitor is read from
  gnome-session's source only. The writer recognises GDK's lost-display lines by GTK
  3.24.52's wording; a GTK that rewords them falls back to the unsaved exit. Where:
  `apps/desktop/src-tauri/src/session_end.rs`,
  `apps/desktop/src-tauri/src/display_lost.rs`; the shutdown items under "Pipeline and
  services (WP6b)". Found: #185, #203, #220.
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
- **First Linux release.** The PipeWire backend's differences from the Mac's: the
  system lane is the whole default sink (Steno's own output included; leaving it out
  was weighed and not done, see the note).
  Where: the Linux items under "Audio". Found: #166, #176.
- **First Linux release.** The capture's latencies are unmeasured on real hardware:
  Nicolai runs "Measuring the latencies on real hardware" in the Linux list under
  "Audio" on a laptop with ALSA and with a Bluetooth headset. Found: #176.
- **First Linux release.** A default move can go unreported on PipeWire before 1.6.9
  (pipewire#5445, fixed upstream by 06de0ed2 on `master` and b784720b on `1.6`; seen
  with 1.6.5 and WirePlumber 0.5.14). Check the versions the release's distributions
  ship; for one on 1.4, file the backport request whose text is ready (not filed); on
  1.0 or 1.2 only the distribution's own package can carry the fix. Where: "A default move can go
  unreported" in the Linux list under "Audio". Found: #197, #201.
- **First Linux release.** Meeting detection on PipeWire names a holder by its binary
  (`application.process.binary`, else `application.name`), so WP9b's prompt needs a
  display name for it, from the app's `.desktop` entry or `application.name`. A
  Flatpak app's pid is its pid inside the sandbox, so two sandboxed apps can merge
  into one holder; check with a Flatpak browser on a real desktop, where
  `pipewire.access.portal.app_id` may name the app better. `MeetingDetector::start`
  fails while PipeWire is unreachable (its first snapshot answers the error), so the
  services (the controller of S2 in `.plans/2026-10-07-stable-promotion.md`) must
  retry it when PipeWire comes up after Steno (autostart at login). Where: "Meeting
  detection" in the Linux list under "Audio". Found: #222.
- **First Windows release.** Gate G4 is open: no Windows machine with a GPU has
  measured DirectML's speed (at least three times the CPU's on an integrated GPU), so
  `directmlOnWindows` stays off by default (`SpeechSettings` in
  `crates/steno-speech/src/runtime.rs`). The same machine checks that DirectML's
  FLEURS transcripts match the CPU's, which nodes ONNX Runtime leaves on the CPU, and
  which event providers a session uses. Before the default flips, the Windows bundles
  must also carry the licence notice of the `DirectML.dll` they ship
  (`apps/desktop/src-tauri/tauri.release.windows.conf.json`). Where: the WP10b
  paragraph, "Off by default" and "Privacy". Found: #188.
- **First Windows release.** A sidecar killed at its deadline is then waited for
  without a bound (`kill` in `crates/steno-speech/src/sidecar/client.rs`), so a child
  stuck in a GPU driver call could hold the engine's lock; the G4 machine checks
  whether a hung DirectML child exits when killed. Where: "Off by default" under
  WP10b. Found: #188.
- **First Windows release.** A Windows logoff or shutdown gives the app about five
  seconds, less than `SHUTDOWN_PATIENCE` (10 s, `crates/steno-services/src/app.rs`),
  and the shell does not call `ShutdownBlockReasonCreate` while a recording runs, so a
  long save at logoff can be cut off. Whether a logoff ends the speech sidecar before
  the pipeline quits (which would mark its meeting `failed`) is unverified. Where: the
  WP10 paragraph and the shutdown items under "Pipeline and services (WP6b)". Found:
  #185.
- **First Windows release.** No Windows machine has run the WASAPI backend: the
  `--ignored` tests in `crates/steno-audio/tests/live_windows.rs`, a real call over
  process loopback, a device switch while recording, the two-clock slip rate,
  detection by executable name and the device list Settings > Recording now shows.
  Where: the Windows list under "Audio". Found: #175, #186.
- **First Windows release.** The `.msi` and NSIS installers are not code-signed (no
  certificate), so SmartScreen asks before the first install. Where:
  `.github/workflows/desktop-release.yml`, the WP9a paragraph. Found: #184.
- **First Windows release.** WebView2 keeps its own browser keys: on the pages that
  do not bind them (Settings, onboarding) Ctrl+Shift+R reloads the page and Ctrl+F
  opens WebView2's find bar. Tauri 2.12 does not expose wry's switch for them
  (`AreBrowserAcceleratorKeysEnabled`); untested on Windows. Where:
  `apps/desktop/src-tauri/src/windows.rs`, `apps/macos/web/src/lib/platform.tsx`.
  Found: #204.
- **Unowned.** The CoreML backend still has its own chunker, merge and decoder
  configuration; moving it onto the shared chunker settles the unticked items of the
  WP4 integration notes (decode loop, merge, recovery, chunking, names). Each backend
  keeps its own until then, so FLEURS and the Swift parity both hold. Where:
  `crates/steno-speech-coreml`, `crates/steno-speech`; the integration notes under WP4.
  Found: #171, #182.
- **Unowned.** The diarizer's ONNX inference runs in the app's process on every
  platform, against invariant 4, so a crash in ONNX Runtime there ends the app; moving
  it needs its own request in the sidecar protocol. Where: `crates/steno-diarize`; the
  "Open, against invariant 4" item under "Pipeline and services (WP6b)". Found: #183.
- **`fix/recovery-adopts-orphans`.** Orphan masters, both apps: a recording's master
  in the audio folder with no meeting row, which neither app cleans up or offers for
  re-import. The phone intake copies the master before its commit, so a crash between
  the copy and the commit leaves one. So does a failed admission commit whose `failed`
  save fails too: the copy stays because Linux keeps a page whose fsync failed in its
  cache, marked clean, and recovery after a crash can bring the admission back with
  it; when recovery does not, the copy is an orphan. So does the re-admission of a
  `complete` receipt whose meeting is missing (the Handover "Admission" item): the
  earlier admission's copy stays beside the new meeting's. The phone keeps its copy
  in every case, so the orphan costs disk space, not a recording. The capture
  recovery's scan of audio folders without a meeting row is to adopt them. Where:
  `RecordingIntake::admit` in `crates/steno-pipeline/src/intake.rs` and
  `RecordingIntake.admit` in `Sources/StenoCore/Storage/RecordingIntake.swift`; the
  Store item on `RecordingIntake.admit`. Found: #213.
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
| WP9b Mac cutover (`.plans/2026-10-04-mac-cutover.md`), ordered by the stable promotion (`.plans/2026-10-07-stable-promotion.md`) | | | planned |
| Services on the speech sidecar: the platform policy, the release after each job, the speech settings | `fix/rust-services-sidecar` | #183 | merged |
| fp32 Parakeet export downloads from Hugging Face (`nicolaischmid/steno-models`) | `feat/rust-host-parakeet-export` | #189 | merged |
| WP10b DirectML for the speech encoder on Windows, behind a probe | `feat/rust-directml` | #188 | merged |
| WASAPI follow-ups: slip window and immediate slip, trusted stream sizes, start deadline, detector start and stop serialised (`steno-audio`) | `fix/rust-wasapi-followups` | #186 | merged |
| Every exit saves first, snapshots on the main thread, the recorder's toggle and the services runtime fixed | `fix/desktop-exit-and-deadlock` | #185 | merged |
| Revoke during a `complete`, a verify bound to the file it hashed, no fixed sleeps in the handover tests | `fix/rust-handover-revocation-flake` | #190 | merged |
| Speech sidecar follow-ups: download lock file, 64 MiB chunks, odd range answers, crash-report stderr, idle child replaced | `fix/rust-sidecar-followups` | #187 | merged |
| Rust CI green on all three platforms after the first merges | `fix/rust-ci-main` | #168 | merged |
| macOS panel size check held to what AppKit allows, panels kept non-activating | `fix/desktop-macos-panel-size` | #178 | merged |
| #154 ported: the cleanup prompt's speaker-label rule and echoed labels stripped (`steno-llm`) | `fix/rust-port-154-cleanup` | #179 | merged |
| The privacy rule names every network path (`AGENTS.md`) | `docs/privacy-network-paths` | #180 | merged |
| #154 ported: the room fallback for a call whose tap carried nothing (`steno-pipeline`) | `fix/rust-port-154-room-fallback` | #181 | merged |
| A phone revoked mid-upload cannot complete it (Swift core, the counterpart of #190) | `fix/handover-revoke-race-swift` | #191 | merged |
| The stop-waits-for-start session test forces its interleaving (`steno-audio`) | `fix/rust-session-race-test` | #194 | merged |
| Each platform's own wording and shortcuts: the platform from the shell, the page's words and keys, the host's permissions and sentences, the vault the CLI named | `fix/desktop-platform-wording` | #204 | merged |
| Every handover engine write in the order asked for: the revoke's delete, the pairing's save and the touch join the receipt saves (`steno-handover`) | `fix/rust-handover-device-writes` | #207 | merged |
| A receipt change is made to the copy memory holds, under the lock that takes its place in line: two chunks that land at once both stay, a `complete` receipt stays `complete` (`steno-handover`) | `fix/rust-handover-chunk-fold` | #208 | merged |
| A `complete` receipt stays `complete`, a racing first announce answers as a re-announce, and a revoked device's late write leaves another device's receipt alone (Swift core, the counterpart of #208) | `fix/swift-handover-complete-stays` | #209 | merged |
| Small fixes after the port: non-Unicode environment variables, `steno process --title` as the user's title, logs that never wait for stderr, the headless PipeWire script's socket paths, a pnpm setup directory per CI job | `fix/rust-small-after-port` | #202 | merged |
| Only a 401 to the current pairing's token unpairs the phone (`mobile/`) | `fix/mobile-current-pairing-401` | #200 | merged |
| A `complete` answered after an unpair, or after a new pairing, still delivers the recording and deletes the phone's copy (`mobile/`) | `fix/mobile-complete-after-unpair` | #211 | merged |
| A call's folder note names the platform it was recorded on ("Windows call", "Linux call"); the `source` key stays `mac-call` (`steno-adapters`) | `fix/adapters-platform-call-label` | #215 | merged |
| The handover's admission, first announce and revoke refusals leave another device's files and receipt alone, and the admission leaves no file behind (`steno-handover`) | `fix/rust-handover-admit-announce` | #219 | merged |
| No traffic light inset under a native title bar: the sidebars' spacer and onboarding's top follow the platform (`apps/macos/web/`) | `fix/web-platform-title-inset` | #217 | merged |
| After the intake, `admit` discards every file of the recording unless another device holds its receipt, and so do a refused `complete` and a failed first save (Swift core, the counterpart of #219) | `fix/swift-handover-admit` | #212 | open |
| The phone rebuilds its queue index from the recording files on disk and the recorder's directory, gives every recording its own file, saves nothing after a failed load, and fails a row after repeated announce 409s (`mobile/`) | `fix/mobile-queue-index-rebuild` | #223 | open |
| A re-announce of a `complete` recording with another size or hash is answered 409, so the phone keeps its file, and the same bytes in other chunks are answered `complete` (`steno-handover`, Swift core) | `fix/handover-reannounce-hash-check` | #224 | open |
| A first announce, one that finds no receipt in memory or the store, discards every inbox file of the recording id before it opens its own, so an old verified file is never admitted unhashed; Swift's announce answers a failed receipt read with 500 (`steno-handover`, Swift core) | `fix/handover-first-announce-discard` | #239 | open |
| The decoder and the mixdown stream each lane in bounded blocks, a sidecar is taken only at the master's length, launch recovery gives up on a meeting whose processing keeps ending the app, and `reprocess` lands (`steno-audio`, `steno-pipeline`) | `fix/decoder-streams-lanes` | #228 | open |
| The phone intake's receipt, meeting and asset commit in one durable transaction before `complete` answers, completing only the admitting device's receipt; a failed commit keeps the copy until a durable `failed` receipt is saved; a `complete` receipt without its meeting is not admitted; pairings and revokes are durable; the handover starts only after a durable checkpoint at launch (both apps) | `fix/handover-durable-intake` | #213 | open |
| Linux input device list and meeting detection over PipeWire, the services reading every platform's device list, a missing chosen microphone recording the default input on every platform (with a warning naming the microphone in use, and a return once it is back and opens; one that does not open waits for the next rebuild), `start`'s first-cycle wait settled, the latency steps for real hardware | `fix/linux-devices-and-detection` | #222 | merged |
| On Windows the durable writes (`replace_file`, `copy_durably`, `create_dir_all_durably`, `create_new_dir_durably`) rename written through (`MoveFileExW` with `MOVEFILE_WRITE_THROUGH`) or with std, then flush the renamed file and the folders; a failed flush answers the phone 500 or fails the export, a drive that refuses a folder flush is passed over, and Settings warns under an audio folder on a drive that is neither NTFS nor ReFS or on a network drive; the phone intake never writes into a meeting folder it did not create (`steno-pipeline`, `steno-adapters`, `steno-host`, web UI) | `fix/windows-durable-rename` | #242 | open |
| Schema v5's admission ledger: an announce of admitted bytes is answered delivered after a revoke or a meeting delete, other bytes under a recording id are a new recording, the same bytes from another device take the receipt over and are admitted once, the same bytes in another split restart the partial; the migrator ignores later migrations and the desktop shows a dialog when the store cannot be opened (both apps) | `fix/handover-lost-complete-answer` | #243 | open |
| A device that will not run at 48 kHz (a headset in the hands-free profile) is recorded at its own rate and converted to 48 kHz on the processing thread, at start and after a switch mid-call, instead of failing (`steno-audio`, Swift core) | `t3code/check-rust-audio-sample-rate` | #198 | open |
| Settings warns under an audio folder on a network mount on Linux and macOS too (`statfs`: NFS, SMB, VM host shares and remote FUSE mounts on Linux; a mount without `MNT_LOCAL`, SMB, NFS, AFP, WebDAV and macFUSE on macOS; `steno-pipeline`) | `fix/network-folder-warning` | #245 | open |
| The Bonjour record follows a network change on every platform, and the Mac advertises its computer name (`steno-handover`, `whoami` 2) | `fix/handover-republish` | #247 | open |
| On Windows two writers of one path in the process rename and flush one after the other, and std's rename and the reopen for the flush are retried on a sharing or lock violation or "access denied" for about 0.9 s, so a durable replace no longer fails because of another writer's flush (`steno-pipeline`) | `fix/windows-parallel-replace` | #252 | open |
| The phone resolves the computer again after a request fails to connect and every 30 s while uploads are queued in the foreground, keeps the address in use while it answers, also after a relaunch, and cancels the chunks still out to an address that stopped answering (`mobile/`, `use-upload-coordinator`, `adopted-origin`) | `fix/mobile-re-resolve` | #254 | open |
| On Windows the other renames of a file Steno writes share #252's busy-file retries (`steno_core::busy_file`): the vault writer's rename and reopen, the handover inbox's promote and metadata, the Codex sign-in file, the model downloads, `files::set_aside` and the speaker clips' staged writes and moves into place; speaker clips written all or none are a follow-up (`steno-core`, `steno-pipeline`, `steno-adapters`, `steno-handover`, `steno-llm`, `steno-speech`) | `fix/windows-rename-retry` | #256 | open |
| Speaker clips under per-run names (`speakers/<SPEAKER-UUID>-<RUN-UUID>.wav`), written durably and only into the meeting's own folder before the merge, named in the merge's durable transaction that keeps the confirmations, and after that commit the clip files of the meeting's speakers that no row names swept from that folder while the run holds the meeting in the in-flight set; a confirmed speaker the re-run gives no clip keeps its clip, and a dropped one its files, and retention also removes the unnamed ones: a run that fails or ends at any point leaves each speaker naming a whole clip (`steno-core`, `steno-pipeline`) | `fix/per-run-speaker-clips` | #257 | open |
| On Windows the credential store keeps the API key and the handover identity on this computer (`CRED_PERSIST_LOCAL_MACHINE`) instead of with the roaming profile, and moves the `keyring` crate's roaming credentials there on their first read, keeping the stored blob under `CRED_PRESERVE_CREDENTIAL_BLOB` (`steno-services`) | `fix/windows-credential-persist` | #262 | open |
| A lost display saves first, the portal's session monitor and logout inhibitor, a logout called off keeps recording, Xfce on Wayland saves at the query, Xfce's Quit Program saves and its Save Session records on, Settings and onboarding kept on Linux (#160) | `fix/desktop-linux-session-end` | #220 | in review |
| Stable plan A9: the AAC priming trimmed from the edit list, the gapless tag or, in the phone recorder's layout alone, AVFoundation's default; the resamplers' sweep and the FLEURS 44.1 against 48 kHz comparison; the macOS capture's first callback logged against its start; `steno dev onsets`, and `record --keep-raw-mic` naming `mic.raw.caf` (`steno-audio`, `steno-speech` tests, `steno-cli`) | `fix/a9-final-audio-choices` | #246 | open |
| S4: the update schedule (a daily check over the Tauri updater, the automatic-check and automatic-download flags in `preferences.json`, the last check time in `update-check.json`, a 60 s limit per check, no announcement while a recording starts, runs or stops, a second confirm before a yes ends one and no install over a recording started during the download, recording starts held off from the install through the relaunch, downloads and installs by itself only through the P25 install gate and so none until P25, none for a packaged install) and the pairing QR code drawn as a PNG (`steno-services`, `steno-host`, desktop shell) | `feat/rust-update-schedule` | #258 | open |
| The flake builds the Linux app from source (`packages.x86_64-linux.steno`: nixpkgs' ONNX Runtime, the tray's `dlopen` patched, the sidecar beside the wrapped binary, `STENO_DISTRIBUTION=nix`) and adds the NixOS module `programs.steno` (`steno.service` with the graphical session, which a rebuild never restarts or stops, PipeWire, GNOME Keyring where no other Secret Service or SSH agent runs, opt-in logind delay; the firewall is X4's), X7 of `.plans/2026-10-07-stable-promotion.md`; FLEURS 4.9 % with either ONNX Runtime build (`flake.nix`, `nix/`) | `feat/nix-linux-package` | #259 | open |

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
decoder; PipeWire and WASAPI were stubs until WP5b and WP10a (below)
replaced them. The zero-allocation
proof is `crates/steno-audio/tests/realtime.rs`; the ERLE table is
identical to Swift's `aec-bench --synthetic`; the ring tests run under
ThreadSanitizer in CI's `tsan` job; the live Core Audio tests sit behind
`--ignored` in `tests/live.rs`. Parity items: the Audio list above.

What WP7c left for the next package: `crates/steno-handover` is a rustls (ring)
listener, TLS 1.3 only, hyper 1 HTTP/1.1, with the pinned verifier (`pinning`), the
rcgen identity in the `SecretStore` as one PEM bundle, pairing, the seven routes, the
inbox and the mdns-sd advertiser; `tests/wire_contract.rs` reads `wire.ts`. The store
gains the paired-device and handover-receipt queries. The intake (copy into the audio
folder, enqueue) arrived with WP6b as `RecordingIntake` in
`crates/steno-pipeline/src/intake.rs`; the audio folder's path comes from
`paths::file_url_path`, the meeting's folder from `RecordingLayout`. Durability before
`complete` answers 200 is the intake's, as in Swift: the listener fsyncs each chunk
(`receiving_file::write`) and writes its own `complete` receipt only after
`HandoverIntake::admit` returns. The intake syncs the copy and its folder first
(`steno_pipeline::files::copy_durably`), and its receipt, meeting and asset commit in
one durable transaction in both apps (the `RecordingIntake.admit` line under Store).
The pairing's save and the revoke's delete are durable in both apps too.

WP5b is the Linux `LiveCaptureBackend`, `crates/steno-audio/src/capture/live/pipewire/`:
one PipeWire capture stream (48 kHz `f32`, one `AUXn` channel per linked
port) that Steno links itself, through the server's `link-factory`, to the
microphone's first output port and the default sink's front monitor ports.
Every graph cycle brings all lanes in one interleaved buffer, which goes
through `deliver_slices`, the safe form of the `deliver` the Mac's IOProc
calls (the view type it shares with WP10a's stream bodies); the stream's `process`
runs on PipeWire's data-loop thread. Default device moves, a node or
port the capture reads going away, a failed or removed link (lost for
the lane it serves) and a lost connection or stream (lost for both) are
coalesced for 500 ms (2 s at most from the first) and judged with
`DeviceSnapshot::difference` against the devices the targets resolved
to, as on the Mac. The proof: `tests/realtime.rs` counts the process
body on every OS, and `tests/pipewire.rs` runs against a private
headless daemon with WirePlumber and null devices
(`scripts/pipewire-headless.sh`, a step of the Linux CI job): each lane
carries its own tone, PipeWire's data-loop thread makes zero allocations
over a second of cycles, `stop()` leaves no thread, no node and no frame
behind, the device changes are reported once per burst, no sooner than
the coalescing delay and also while other apps' streams keep coming and
going, the rebuild's restart runs, a monitor link removed from outside
reads as the output gone and so do both links removed (the monitor's
first), the microphone's link removed from outside or a microphone that
vanishes during a call reads as the input gone, the capture's connection
closed from outside reads as the output gone (the input in person), and
changes that settle back or touch other nodes are not reported.
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
(`capture-spike` and `audio-devices` run on the Mac and on Linux, `audio-devices`
on Windows too); one `build()`
assembles the graph; the shell's `fixture-host` is opt-in. Parakeet's ONNX engine ran
in the app's process until #183 moved it into the speech sidecar; the
`CoreML` engine leaves `language` unset (#163), and
`LanguageTaggingEngine` in the services crate runs `steno_speech`'s tagger after it,
as `ParakeetMapping` did in Swift. Secrets: the platform keyring on macOS and
Windows, the Secret Service on Linux, or the 0600 `secrets.json` where no keyring
answers before the first move into it (the kernel keyring does not survive a reboot).
Parity items: the Pipeline and services list above.

WP10a is the Windows half of `crates/steno-audio`: the WASAPI live backend
(process loopback excluding Steno's process tree, endpoint loopback as the
fallback, the capture endpoint, one thread per stream, endpoint
notifications and the rebuild report) and the session-based
`LiveProcessAudioActivity`. Not run on hardware: built, linted and
tested on the `windows-latest` runner, where only process loopback (and
no microphone) can run.
The per-packet bodies, the stream plan and the session mapping are
platform-independent (`tests/split_streams.rs`, `tests/sessions.rs`), the
zero-allocation proof covers both stream bodies (`tests/realtime.rs`), and
the hardware checks wait behind `--ignored` in `tests/live_windows.rs`.
Parity items: the Windows list under Audio.

WP10b puts the speech encoder on DirectML on Windows when the speech setting
`directmlOnWindows` asks for it (`OnnxOptions::directml`, set from the setting by
`steno_services::speech::SpeechSetup` and carried to the sidecar in its load
request, on Windows only; the child answers with the provider it chose). Only the
encoder moves: the decoder and the joiner run once per token on one frame, and
Silero on 32 ms frames, where a round trip to the GPU costs more than the step;
the diarizer stays on the CPU because it runs in the app's process, where
speech-stack decision 5 keeps no GPU driver. The probe is the session itself:
DirectML in the ONNX Runtime build, a hardware DirectX 12 adapter (the device
filter leaves out WARP), the session created, one encoder run on a second of
silence. Any failure opens the encoder on the CPU, and a later run that fails on
DirectML reopens it on the CPU for good and runs again, so the job does not fail.
A child whose CPU reopen fails too exits without answering, which counts as a
crash on DirectML.

An abort inside the driver still ends the sidecar. A child that crashes, hangs or
overruns the memory ceiling during a load or a request with DirectML in use
switches DirectML off for the rest of the app's run: the switch is process-wide,
so the sidecar engine `steno-services` keeps across pipeline reloads, and any
other in the process, asks for the CPU too. A child that dies between requests is
replaced on DirectML, as nothing ran on it since its last answer; one that
overruns the ceiling between requests still switches DirectML off, as what it
holds then is what its last request left, on the GPU too. Inside the probe such
an end costs no job: no audio was sent yet, so the same call loads again in a new
child on the CPU. Mid-run it costs that job. The
provider is logged at info level, with no paths: by the backend when it runs
in-process, and by the parent from the child's answers, which carry the provider
in force, so a fallback after the load shows in the parent's log and in
`SidecarHealth`. The child has no log subscriber; its stderr reaches the parent's
log (at debug level, the fallback line at info) and the crash tail, so the child
writes the reason for a fallback there in fixed words. A killed child is logged at
warn level with its exit status and the kind of failure only; the error with the
crash tail goes to debug. What is and is not proven:

- **Off by default.** Gate G4 (at least three times the CPU's speed on an
  integrated GPU) is open with no machine, and so is whether DirectML's
  transcripts match the CPU's on FLEURS (gate G1 was measured on the CPU). The
  default flips when a Windows machine with a GPU has measured both. The
  `DirectMl` label means the provider is registered for the encoder's session;
  ONNX Runtime may still place nodes DirectML does not support on the CPU, which
  the measurement checks in ONNX Runtime's verbose session log. A child that
  hangs is killed at its deadline, but the kill waits for the child's exit
  without bound, so a child stuck in a driver call could hold the engine's lock;
  the measurement checks whether a hung DirectML child exits when killed.
- **The CPU path is unchanged.** With the setting off, and on Linux and macOS
  where it is ignored, the ten FLEURS German `cat/` files give segments and tokens
  byte-identical to `main`'s on atlas (`STENO_MODELS_DIR` with the fp32 export).
- **The fallback runs.** On the `windows-latest` runner (no GPU), the "DirectML
  probe, with output" step finds DirectML in the build, DirectML refuses to start
  because no device matches the default filter (`NoDevice`), and the session runs
  on the CPU; the step fails if the log says otherwise. Under wine 11 with the MSVC
  build (`cargo xwin`) the probe ends the same way.
- **After a failure.** A failed run moving to the CPU is tested on a hand-written
  ONNX model with a CPU session labelled `DirectML`. The switch-off is tested with
  the fake engine on Windows: an abort on DirectML leaves the next child and every
  later engine on the CPU, an abort inside the probe loads again on the CPU within
  the same call, a child that lost its encoder on DirectML exits unanswered and
  switches DirectML off, a child over the ceiling between requests on DirectML
  switches it off too (`directml_idle_overrun.rs`), and a child killed between
  requests on DirectML (`directml_idle_death.rs`), an error, a release or a crash
  after a fallback to the CPU leaves DirectML on. On every platform, a crash inside
  a load on the CPU is not retried within the call. No real GPU fault has run.
- **`DirectML.dll` is a load-time import** of every Windows binary that links
  ONNX Runtime, with or without this package: pyke publishes only DirectML builds
  of ONNX Runtime for Windows, and `ort-sys` links `DirectML.lib` for them. ONNX
  Runtime calls `DMLCreateDevice1(.., DML_FEATURE_LEVEL_5_0, ..)`, which needs
  DirectML 1.8. The bundles from #184 install `DirectML.dll` beside the binaries,
  so the app does not depend on the copy in `System32`. A binary without the DLL
  beside it loads that copy, which is 1.8 only from Windows 11 22H2 (Windows 10
  2004 has 1.1, Windows 11 21H2 has 1.6): from Windows 10 2004 the process starts
  but DirectML falls back with `NoDevice`, and on 1903 and 1909 the process does
  not start at all. The G4 machine needs the DLL beside the binary. Neither case
  has run on Windows.
- **Privacy.** ONNX Runtime's telemetry stays off. With DirectML on, `DirectML.dll`
  and Direct3D 12 may log to Windows' own diagnostic data as for any program that
  uses them; Steno opens nothing for it, and no audio or text is involved. Before
  the default flips, the measurement records which event providers a DirectML
  session uses, and the bundles are to carry the redistributable's licence notice,
  which they do not yet.
- **Not done:** CUDA on Linux and the whisper.cpp Vulkan engine (speech-stack
  WP4), a device choice (the system's default GPU is used), and the setting in
  the Settings window (the parity item under Speech).

Parity items: the Speech list.
