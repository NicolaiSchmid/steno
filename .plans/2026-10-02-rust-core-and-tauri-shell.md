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

Not in this plan: the diarization rebuild (speech-stack WP3, gate G3), GPU execution
providers (WP4, gate G4), the iOS recorder (unchanged), and any new product feature.
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
   from `spikes/capture-rs/src/rt.rs` in a test build.
6. **Scope stays the scope.** No new features in the port; a Rust behaviour that
   differs from Swift is a bug unless a plan says otherwise.

## Architecture

```
Cargo.toml                 workspace
crates/
  steno-core/              domain types, protocols (the pluggable boundaries), SQLite store and migrations, pipeline orchestration, settings
  steno-bridge/            the JSON contract (topics, methods, snapshots, params, envelope), fixture tests
  steno-audio/             capture backends (CoreAudio taps, PipeWire, WASAPI), ring buffer, AEC, writer
  steno-speech/            VAD, chunker, merge, TDT decoder; CoreML and ONNX Runtime backends; model store
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
no logic: view models and the bridge host live in `steno-core` (host module) so the
CLI, tests and the shell share them, which is the extraction the spikes plan asked for
in Swift and which happens in Rust instead. One temporary exception: from WP3 until
WP6 the shell carries a fixture host behind its `fixture-host` feature that answers
the bridge from the recorded fixtures, so the UI runs on every platform before the
pipeline exists.

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
  **WP5 audio.** `steno-audio` from `spikes/capture-rs`: CoreAudio backend with
  device-change rebuild, synthetic backend, writer, AEC; capture tests from
  `Tests/StenoAudioTests` ported. PipeWire backend. WASAPI backend last.
- **WP6 pipeline and host.** Orchestration (`process`, retention, speaker matching,
  export), the host module with view models and the bridge host over the real store,
  the CLI; the shell's `fixture-host` feature is switched off and removed. Parity: the
  Swift `steno export` of a calibration meeting equals the Rust one field for field.
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

Filled in by WP6 from the Swift app's bridge methods and topics (one line per method and
topic, plus menu bar, panels, deep links, auto-stop, detection, retention, updates,
login item, calendar, phone pairing). Until then, only what the first crates turned up.

### Store

- `StenoJSON` date output truncates to the millisecond; Rust rounds like GRDB; fix the
  Swift formatter before cutover.
- `MeetingStore.init` should check the migrator's `hasBeenSuperseded` and refuse a
  database with an identifier it does not know, as the Rust store does
  (`StoreError::UnknownMigration`); today GRDB ignores unknown identifiers and the
  Swift app would run on a newer schema without noticing.

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
  first one (folder pin lost, stale lines never removed).
- The person-page writer should write file names NFC-normalised, as the Rust writer
  does; Foundation writes `Anna Müller.md` in NFD on APFS, which maps both to one
  file, but a vault synced to a normalisation-sensitive filesystem gets two files.

### Bridge

What the bridge crate (WP1) asks of the Swift side before WP6 fills the list:

- Fixtures: one `<topic>.full` fixture per snapshot topic with every optional field
  set, written by `BridgeSamples` next to the existing ones, so the Rust round-trip
  test pins the optional keys that `crates/steno-bridge/tests/optional_fields.rs`
  pins by hand today.
- Fixtures: a task with `priority: low` in `meeting.detail`, so the round-trip pins
  every `TaskPriority` case; `contract_ts_nested_enums_match` in
  `crates/steno-bridge/tests/fixtures.rs` pins it by hand today.

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
| WP7b adapters | `feat/rust-adapters` | #165 | open |

WP4b is `crates/steno-speech-coreml`: `objc2-core-ml` behind one safe module,
the four backend calls, the FluidAudio 0.17.4 heuristics ported
(silence-aligned starts, contiguous-match merge, seam-word collapse, seam-gap
repair, suppressed-token gate), four parallel windows, `SpeechEngine`
implemented, parity harness `steno-coreml-parity`. Inverse text normalisation
is not applied: Steno's Swift path (`ParakeetEngine` to
`AsrManager.transcribe`) never calls FluidAudio's `TextNormalizer`, so the
baseline carries none. Parity numbers: see the PR.
