# Steno core foundation: package, StenoCore, `steno` CLI skeleton

Status: implementation plan, 2026-09-25, reconciled the same day. Workstream
1 of [`2026-09-25-v1-program.md`](2026-09-25-v1-program.md); scope authority
is [`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md). Model,
protocol and supporting-type shapes below are the program's; this plan adds
storage, pipeline and CLI detail only.

## Goal

Create the Swift package every other workstream compiles against: the
canonical model, GRDB storage with one append-only migration file and FTS5,
the program's protocols plus supporting types, a `ProcessingPipeline` whose
ten steps are injectable values, the template registry with the four fixed
templates as bundled JSON, settings persistence, the meeting event bus, the
shared test support (`Testing/`: fakes, fixture access, golden snapshots), and
a `steno` CLI that can migrate a database, run the pipeline against a WAV file
with fakes, and export a meeting as JSON. When this merges, audio, speech, LLM,
adapters and handover proceed in parallel against compiled protocols and a
green `macos-15` CI job.

## Non-goals

- Any real capture, STT, diarization, LLM or destination implementation.
  Fakes only, no network, no model downloads.
- Decoding compressed audio, resampling or AAC mixdown. Core's
  `WAVAudioDecoder` reads 16 kHz mono PCM WAV for fixtures and the CLI; the
  real `AudioDecoding` is StenoAudio's (`2026-09-25-audio-capture.md`).
- Markdown and VTT rendering and delivery (`2026-09-25-adapters-obsidian.md`);
  `steno export` writes `meeting.json` only. Keychain access (the macOS app
  owns the `SecretStore` implementation).
- SwiftUI, xcodegen, Sparkle, signing, the Forge runner, and any schema
  beyond the program's canonical model.

## Decisions

- `swift-tools-version: 6.1`, `platforms: [.macOS(.v15)]`,
  `swiftLanguageModes: [.v6]`. GRDB 7.11.1 requires Swift 6.1 / Xcode 16.3+;
  the `macos-15` runner defaults to Xcode 16.4 and offers 26.x.
- Dependencies declared now: GRDB.swift `from: "7.11.1"` and
  swift-argument-parser `from: "1.8.2"`. FluidAudio, WhisperKit, CSpeex,
  swift-certificates and Sparkle are added by their own workstreams.
- swift-argument-parser: nested subcommands (`db migrate`, `fixtures
  generate`), typed options, `--help` and `AsyncParsableCommand`; hand-rolled
  parsing would be rewritten the first time the bake-off needs a flag.
- `.enableUpcomingFeature("InferSendableFromCaptures")` on every target, as
  GRDB documents for Swift 6 shorthand closures. Tests use Swift Testing
  (`@Test`, `#expect`) on an in-memory `DatabaseQueue()`; app and CLI use
  `DatabasePool(path:configuration:)` (WAL).
- Records are structs, `Codable + FetchableRecord + PersistableRecord`, hence
  `Sendable`. `MeetingStore` is a `Sendable` final class holding
  `let writer: any DatabaseWriter`, not an actor: the pool already serialises
  writes and an actor would serialise reads too.
- Column encodings: `UUID` as uppercase TEXT, `Date` as GRDB's default UTC
  text, `[String]` as `.jsonText`, embeddings as 1024-byte little-endian
  Float32 BLOB through `Embedding: DatabaseValueConvertible`,
  `Locale.Language` as its BCP-47 identifier, `DeliveryReceipt` and
  `LLMUsage` as JSON text. Enums with payloads (`MeetingState.failed`,
  `AudioRetention.keepDays`, `DeliveryStatus`, `HandoverReceipt.state`)
  flatten into two columns each in private row structs in `Storage/`;
  canonical types stay clean for `meeting.json`.
- Tables keep the implicit rowid (never `withoutRowID`) because FTS5 external
  content tables join on rowid. The app never runs `VACUUM`;
  `steno db reindex` rebuilds the FTS index if it drifts.
- One migration `"v1"` creates everything. `eraseDatabaseOnSchemaChange`
  stays `false` everywhere; append-only discipline instead.
- Templates are data (id, displayName, description, context, sections with
  id, heading, instructions, required) in
  `Sources/StenoCore/Resources/Templates/*.json` behind `Bundle.module`.
  Prompt wording is StenoLLM's.
- `Sources/StenoCore/Testing/` holds every fake, `Fixtures.url(_:)` and
  `Snapshot`, public, so all test targets and the CLI share them. Fixture
  folders under `Tests/Fixtures/` are lowercase (`audio`, `transcripts`,
  `templates`, `exports`, ...) because APFS is case-insensitive by default.
- `MeetingStore` conforms to `PersonStore` and `HandoverStore`;
  `RecordingIntake` implements `HandoverIntake` on top of the store and the
  pipeline. No separate storage classes per consumer.

## Public API

Canonical model exactly as listed in the program (`Meeting`, `Participant`,
`Person`, `Embedding`, `Speaker`, `TranscriptSegment`, `MeetingTask`,
`Decision`, `AudioAsset` with `AudioFormat`, `AudioLane`, `AudioRetention`,
`Delivery`, `DeliveryStatus`, `DeliveryReceipt`, `PairedDevice`,
`HandoverReceipt`, `LLMUsage`, `Settings`), all `Codable, Sendable,
Equatable, Hashable`, row types `Identifiable`. Protocols exactly as in the
program: `SpeechEngine`, `Diarizer`, `EchoCanceller`, `LanguageModel`,
`Destination`, `SpeakerMemory`, `PersonStore`, `AudioDecoding`,
`TranscriptCleaner`, `MeetingSummarizer`, `DeliveryDispatcher`, `SecretStore`,
`HandoverStore`, `HandoverIntake`. Supporting types as in the program; the
shapes that need spelling out:

```swift
public struct AudioBuffer16k: Sendable, Equatable { public static let sampleRate: Double = 16_000; public var samples: [Float]; public var duration: TimeInterval }
public struct RawSegment: Codable, Sendable, Equatable { start, end: TimeInterval; text: String; language: Locale.Language?; wordTimings: [WordTiming]? }
public struct WordTiming: Codable, Sendable, Equatable { word: String; start, end: TimeInterval }
public struct DiarizationResult: Codable, Sendable, Equatable { public var clusters: [SpeakerCluster] }
public struct SpeakerCluster: Codable, Sendable, Equatable { label: String; ranges: [ClosedRange<TimeInterval>]; embedding: Embedding?; confidence: Float; sampleClipRange: ClosedRange<TimeInterval>? }
public struct LLMRequest: Codable, Sendable { messages: [LLMMessage]; responseFormat: LLMResponseFormat; temperature: Double?; maxTokens: Int?; purpose: String }
public enum LLMResponseFormat: Codable, Sendable, Equatable { case text, jsonObject, jsonSchema(name: String, schema: JSONValue, strict: Bool) }
public struct LLMMessage: Codable, Sendable { role: LLMRole; content: String };  public enum LLMRole: String { case system, user, assistant }
public struct LLMResponse: Codable, Sendable { text: String; finishReason: LLMFinishReason; usage: LLMUsage?; model: String? }
public enum LLMFinishReason: String, Codable, Sendable { case stop, length, contentFilter, other }
public indirect enum JSONValue: Codable, Sendable, Equatable, Hashable { case string(String), number(Double), bool(Bool), null, array([JSONValue]), object([String: JSONValue]) }
public struct CleanupContext: Sendable { language: Locale.Language?; participants: [Participant]; speakers: [Speaker]; calendarAttendees: [String]; knownPeople: [Person] }
public struct CleanupResult: Sendable, Equatable { segments: [TranscriptSegment]; failedChunks: [Int]; usage: LLMUsage }
public struct SummaryInput: Sendable { meeting: Meeting; segments: [TranscriptSegment]; speakers: [Speaker]; participants: [Participant]; calendarAttendees: [String]; knownPeople: [Person]; template: SummaryTemplate }
public struct SummaryOutput: Codable, Sendable, Equatable { title: String; summaryMarkdown: String; decisions: [String]; tasks: [MeetingTask]; speakerNames: [SpeakerNameSuggestion]; language: Locale.Language?; usage: LLMUsage }
public struct SpeakerNameSuggestion: Codable, Sendable, Equatable { speakerID: UUID; name: String?; confidence: Double; evidence: String }
public struct MeetingExport: Codable, Sendable, Equatable {
    meeting: Meeting; participants: [Participant]; speakers: [Speaker]; persons: [Person]; segments: [TranscriptSegment]
    tasks: [MeetingTask]; decisions: [Decision]; audio: AudioAsset?; artifacts: [RenderedArtifact]   // filled by StenoAdapters
}
public struct RenderedArtifact: Codable, Sendable, Equatable { kind: Kind; fileName: String; data: Data; personID: UUID?
    public enum Kind: String, Codable, Sendable { case folderNote, transcript, tasks, vtt, json, personPage, audio } }
public struct DestinationSettings: Codable, Sendable, Equatable { public subscript(key: String) -> JSONValue?; string(_:), bool(_:), int(_:) typed getters }
public enum DeliveryMode: Codable, Sendable, Equatable { case initial, reexport(previous: DeliveryReceipt?) }
public struct RecordingMetadata: Codable, Sendable, Equatable { recordingID: UUID; startedAt: Date; durationSeconds: Double; byteCount: Int64; sha256: Data; chunkSize: Int; format: String; deviceName: String }
public struct SummaryTemplate: Codable, Sendable, Identifiable { id: String; displayName: String; description: String; context: String; sections: [TemplateSection] }
public struct TemplateSection: Codable, Sendable, Equatable { id: String; heading: String; instructions: String; required: Bool }
public struct SecretKey: RawRepresentable, Sendable, Hashable { public static let llmAPIKey: SecretKey }
public enum MeetingEvent: Sendable, Equatable {
    case stateChanged(meetingID: UUID, state: MeetingState)
    case progress(meetingID: UUID, stage: String, fraction: Double)          // one per pipeline step
    case meetingReady(meetingID: UUID)
    case speakersNeedReview(meetingID: UUID, speakerIDs: [UUID])
    case deliveryUpdated(Delivery)
}
```

Storage, pipeline, templates, settings, events, intake:

```swift
public struct StenoPaths: Sendable { public static func `default`() throws -> StenoPaths; public var databaseURL: URL; public var supportDirectory: URL }
public enum Migrations { public static func migrator() -> DatabaseMigrator }             // one file, append-only
public final class MeetingStore: Sendable, PersonStore, HandoverStore {
    public init(writer: any DatabaseWriter) throws                                       // runs migrator
    public static func onDisk(at url: URL) throws -> MeetingStore                        // DatabasePool, WAL, busyMode .timeout(5)
    public static func inMemory() throws -> MeetingStore                                 // DatabaseQueue()
    public func save(_ meeting: Meeting) async throws; public func meeting(id: UUID) async throws -> Meeting?
    public func meetings(limit: Int, offset: Int) async throws -> [Meeting]
    public func setState(_ state: MeetingState, meetingID: UUID) async throws
    public func replaceTranscript(meetingID: UUID, segments: [TranscriptSegment], speakers: [Speaker]) async throws
    public func replaceSummary(meetingID: UUID, output: SummaryOutput, templateID: String) async throws
    public func save(_ asset: AudioAsset) async throws; public func asset(id: UUID) async throws -> AudioAsset?
    public func mergeSpeakers(_ source: UUID, into target: UUID, meetingID: UUID) async throws   // in-meeting cluster merge
    public func assign(speakerID: UUID, personID: UUID?) async throws
    public func upsert(_ delivery: Delivery) async throws; public func deliveries(meetingID: UUID) async throws -> [Delivery]
    public func export(meetingID: UUID) async throws -> MeetingExport                    // artifacts empty
    public func search(_ query: String, limit: Int) async throws -> [SearchHit]           // FTS5, bm25 order
    public func observeMeetings() -> AsyncThrowingStream<[Meeting], Error>              // ValueObservation.values(in:)
    public func observeMeeting(id: UUID) -> AsyncThrowingStream<MeetingExport?, Error>
    public func rebuildSearchIndex() async throws
    public func deleteExpiredAudio(now: Date) async throws -> [URL]                      // master, sidecars, mixdown; caller removes files
    // PersonStore: persons(), save(_ person:), mergePersons(keep:remove:)   HandoverStore: six members per program
}
public struct SearchHit: Sendable, Equatable { meetingID: UUID; segmentID: UUID?; snippet: String; rank: Double }
public final class SettingsStore: Sendable {                                              // key/value table `setting`
    public init(writer: any DatabaseWriter); public func load() async throws -> Settings
    public func save(_ settings: Settings) async throws; public func observe() -> AsyncThrowingStream<Settings, Error>
}
public actor MeetingEventBus { public func subscribe() -> AsyncStream<MeetingEvent>; public func post(_ event: MeetingEvent) }
public struct TemplateRegistry: Sendable {                                                // ids: default, customer-discovery, daily-standup, interview
    public static let bundled: TemplateRegistry; public static let defaultTemplateID = "default"
    public var templates: [SummaryTemplate]; public func template(id: String) -> SummaryTemplate?
}
public struct PipelineDependencies: Sendable {
    decoder: any AudioDecoding; speechEngine: any SpeechEngine; diarizer: any Diarizer; speakerMemory: any SpeakerMemory
    cleaner: any TranscriptCleaner; summarizer: any MeetingSummarizer; delivery: any DeliveryDispatcher
    store: MeetingStore; settings: SettingsStore; events: MeetingEventBus; templates: TemplateRegistry
}
public struct PipelineSteps: Sendable {                                                   // `.default` wraps the dependencies; tests swap any step
    decode, transcribe, diarize, matchSpeakers, merge, cleanup, summarize, persist, deliver, retention: any PipelineStep
}
public struct PipelineContext: Sendable { meeting: Meeting; asset: AudioAsset; lanes: [AudioLane: AudioBuffer16k]; raw: [AudioLane: [RawSegment]]; diarization: DiarizationResult?; speakers: [Speaker]; segments: [TranscriptSegment]; summary: SummaryOutput?; calendarAttendees: [String] }
public protocol PipelineStep: Sendable { var stage: String { get }; func run(_ ctx: inout PipelineContext, deps: PipelineDependencies) async throws }
public actor ProcessingPipeline {
    public init(dependencies: PipelineDependencies, steps: PipelineSteps = .default)
    public func process(assetID: UUID) async throws                                      // steps 1-10; queued -> processing -> ready | failed
    public func rerunSummary(meetingID: UUID, templateID: String) async throws          // steps 7-9, .reexport
    public func redeliver(meetingID: UUID) async throws                                  // step 9, .reexport
}
public struct RecordingIntake: HandoverIntake, Sendable {                                  // phone recordings
    public init(store: MeetingStore, settings: SettingsStore, pipeline: ProcessingPipeline)
    public func admit(file: URL, metadata: RecordingMetadata, device: PairedDevice) async throws -> UUID
    // moves the file into audioFolder/<meetingID>/, inserts Meeting(.queued, .phone, title from startedAt) + AudioAsset(.m4aAAC, [.mixed],
    // mixdownURL = url) in one transaction, then Task { pipeline.process(assetID:) }; idempotent on recordingID via HandoverReceipt
}
// Testing/: FakeSpeechEngine, FakeDiarizer, FakeLanguageModel, PassthroughCleaner, FakeSummarizer, InMemorySpeakerMemory,
// RecordingDestination, RecordingDispatcher, FakeHandoverIntake, WAVAudioDecoder (16 kHz WAV only; mixdown copies the file), FileSecretStore (0600 file),
// Fixtures.url(_ relative: String) -> URL, Snapshot.assert(_ data: Data, matches relative: String) (STENO_UPDATE_SNAPSHOTS=1 rewrites)
```

`steno` CLI: `steno db migrate [--db PATH]`, `steno db reindex [--db PATH]`,
`steno process <wav> [--system-lane WAV] [--source mac-call|mac-in-person|phone] [--title T] [--template ID] [--db PATH] [--audio-folder DIR]`,
`steno export <meeting-id> [--out DIR] [--db PATH]` (writes `meeting.json`),
`steno fixtures generate --out DIR` (deterministic sine sweeps and two-tone
"conversations", 2 to 8 s, 16 kHz mono WAV; other workstreams add cases).
`--db` defaults to `StenoPaths.default().databaseURL`
(`~/Library/Application Support/Steno/steno.sqlite`). `Wiring.swift` builds
`PipelineDependencies` from fakes; later workstreams swap real
implementations in behind flags in their own command files.

## Files

```
Package.swift                                   targets, products, deps, swift settings
Package.resolved                                committed
Sources/StenoCore/Model/{Meeting,People,Transcript,Outputs,Audio,Delivery,Handover,Settings,JSONValue}.swift
Sources/StenoCore/Model/Language+Codable.swift  Locale.Language <-> identifier helpers, DatabaseValueConvertible
Sources/StenoCore/Protocols/{SpeechEngine,Diarizer,EchoCanceller,LanguageModel,Destination,SpeakerMemory,Stores,PipelineBoundaries,SecretStore}.swift
                                                program protocols plus their request/response types; Stores = PersonStore, HandoverStore, HandoverIntake
Sources/StenoCore/Storage/Migrations.swift      the only migration file; "v1" registers tables, indexes, FTS5
Sources/StenoCore/Storage/Records.swift         private row structs for payload enums, Columns enums, encoding strategies
Sources/StenoCore/Storage/MeetingStore{,+Search,+Export,+People,+Handover}.swift  store, FTS5, export(meetingID:), PersonStore, HandoverStore
Sources/StenoCore/Storage/{SettingsStore,StenoPaths,RecordingIntake}.swift
Sources/StenoCore/Pipeline/{ProcessingPipeline,PipelineContext}.swift  actor and state machine; context, dependencies, steps
Sources/StenoCore/Pipeline/Steps/*.swift        one file per step (Decode, Transcribe, Diarize, MatchSpeakers, Merge, Cleanup, Summarize, Persist, Deliver, Retention)
Sources/StenoCore/Pipeline/LaneMerger.swift     pure function: lane merge with "me" participant
Sources/StenoCore/Events/MeetingEventBus.swift  MeetingEvent, MeetingEventBus
Sources/StenoCore/Templates/TemplateRegistry.swift   SummaryTemplate, TemplateSection, TemplateRegistry
Sources/StenoCore/Resources/Templates/{default,customer-discovery,daily-standup,interview}.json
Sources/StenoCore/Testing/*.swift               fakes, Fixtures, Snapshot (see Public API)
Sources/StenoCore/Audio/WAVAudioDecoder.swift   RIFF/PCM reader, 16 kHz mono only, 16-bit int and 32-bit float; conforms to AudioDecoding
Sources/StenoCore/Audio/WAVWriter.swift         writes 16 kHz mono 16-bit WAV (fixtures)
Sources/StenoAudio/StenoAudio.swift             placeholder `public enum StenoAudio {}` (same for StenoSpeech, StenoLLM, StenoAdapters, StenoHandover)
Sources/steno/Steno.swift                       @main AsyncParsableCommand, subcommand registration
Sources/steno/{Commands/{DB,Process,Export,Fixtures},Wiring}.swift  subcommands; PipelineDependencies from fakes and flags
Tests/StenoCoreTests/*.swift                    see Tests
Tests/StenoAudioTests/PlaceholderTests.swift    one passing test per placeholder module (same for the other four)
Tests/stenoTests/CLITests.swift                 runs the built binary via Process against a temp db
Tests/Fixtures/README.md                        convention: lowercase folders, sizes, how to regenerate
Tests/Fixtures/audio/*.wav                      generated by `steno fixtures generate`, each under 10 s
Tests/Fixtures/transcripts/*.json               [RawSegment] and [TranscriptSegment] samples (invented text)
Tests/Fixtures/templates/*.json                 expected registry output, snapshot
Tests/Fixtures/exports/meeting-export.json      MeetingExport snapshot for step 2
.github/workflows/swift-ci.yml                  see step 1
```

## Steps

1. **Package skeleton and CI (0.5 d).** `Package.swift` with seven targets,
   seven test targets, placeholders, GRDB and ArgumentParser. `swift-ci.yml`:
   `actions/cache` on `.build` keyed by `Package.resolved`, `swift format
   lint --strict --recursive Sources Tests Package.swift` before build,
   `swift test --parallel`. Acceptance: CI green; seven placeholder tests; a
   test executing `CREATE VIRTUAL TABLE t USING fts5(x)` on `DatabaseQueue()`
   passes (FTS5 present in GRDB's SPM build on the runner's system SQLite).
2. **Canonical model and supporting types (1 d).** All types in `Model/` and
   `Protocols/`, `JSONValue`, `Locale.Language` helpers. Acceptance: a
   round-trip test encodes every model type to JSON and back with equality;
   `MeetingExport` JSON for a synthetic meeting matches
   `Tests/Fixtures/exports/meeting-export.json` byte for byte (sorted keys).
3. **Migration v1 and records (1 d).** Tables `meeting`, `participant`,
   `person`, `speaker`, `transcriptSegment`, `meetingTask`, `decision`,
   `audioAsset`, `delivery`, `pairedDevice`, `handoverReceipt`, `setting`;
   foreign keys to `meeting(id)` with `onDelete: .cascade`; indexes on
   `(meetingID, start)`, `state`, `pairedDevice.tokenHash`; FTS5
   `transcriptSegment_ft(text)` and `meeting_ft(title, summary)` via
   `t.synchronize(withTable:)`, `t.tokenizer = .unicode61()`. Acceptance:
   migrator runs on an empty in-memory queue; `PRAGMA foreign_key_check`
   empty; an inserted segment is found via `FTS5Pattern(matchingAllTokensIn:)`;
   `appliedIdentifiers` equals `["v1"]` so later changes must append.
4. **MeetingStore CRUD, export, search, observation, merges (1 d).**
   Acceptance: unit tests per method; `observeMeetings()` yields a second
   value after a `save`; `search("Jérôme")` matches "jerome" through
   unicode61; `mergePersons` re-points speakers and participants and deletes
   the loser; `mergeSpeakers` moves segments, averages embeddings, deletes
   the source `Speaker`; `HandoverStore` round-trips a device and a receipt.
5. **Settings, secrets, paths, events, intake (0.5 d).** Acceptance: default
   `Settings` has `defaultTemplateID == "default"`, `defaultRetention ==
   .keepDays(30)`, `speakerMatchThreshold == 0.60`, `llmContextTokens ==
   32_000`; save then load round-trips; `FileSecretStore` writes mode 0600;
   `MeetingEventBus` fans one post out to two subscribers; `RecordingIntake`
   with a fake pipeline inserts `Meeting(.queued, .phone)` plus `AudioAsset`
   and returns the same id when called twice with one `recordingID`.
6. **Template registry and resources (0.5 d).** Four JSON templates with
   section headings modelled on Jamie's (`2026-09-25-llm-and-templates.md`
   lists the sections). Acceptance:
   `TemplateRegistry.bundled.templates.map(\.id)` equals the four ids in
   order; every section has non-empty `id`, `heading`, `instructions`;
   `Bundle.module` loads under `swift test` and from the built binary.
7. **Testing support and WAV decoder (0.5 d).** Fakes deterministic and
   configurable (segments per lane, cluster count with sample clip ranges,
   canned `SummaryOutput`), `Fixtures`, `Snapshot`. Acceptance: decoding
   `Tests/Fixtures/audio/sweep-3s.wav` yields 48 000 samples; a 48 kHz or
   stereo file throws `WAVDecodeError.unsupportedFormat`; `Snapshot` fails
   with a unified diff and rewrites under `STENO_UPDATE_SNAPSHOTS=1`.
8. **Pipeline steps 1 to 5 (1 d).** Decode, transcribe per lane
   sequentially, diarize the right lane by `MeetingSource`, match through
   `SpeakerMemory.match` (threshold lives in the implementation), copy each
   cluster's `sampleClipRange` and `confidence` onto its `Speaker`, lane
   merge with deterministic "me" participant for `.mic` only. Acceptance:
   unit tests per step with fakes; `LaneMerger` property test: output sorted
   by `start`, no segment lost, no `.mixed` segment carries the "me" id.
9. **Pipeline steps 6 to 10, state machine, reruns (1 d).** Failures in 6 or
   7 keep the transcript and set `.failed(reason:)`; persist calls
   `decoder.mixdown` for `.caf48kFloat32` assets and skips `.m4aAAC`;
   `rerunSummary` runs 7 to 9 with `.reexport(previous: nil)`; `redeliver`
   runs 9; `progress` posted per step. Acceptance: integration test drives
   `process(assetID:)` end to end on an in-memory store with all fakes and
   asserts the final `MeetingExport`, the `RecordingDispatcher` call with
   `.initial`, `mixdownURL` set, and the event sequence `queued ->
   processing -> progress x10 -> ready, meetingReady`; a throwing
   `FakeSummarizer` yields `.failed` plus intact segments; retention `0`
   sets `expiresAt` and `deleteExpiredAudio` lists master, sidecars, mixdown.
10. **CLI (1 d).** Subcommands as listed, `Wiring.swift`, exit codes (0 ok,
    1 usage, 2 runtime). Acceptance: `Tests/stenoTests` runs the binary:
    `db migrate` on a temp path creates the file; `fixtures generate` then
    `process sweep-3s.wav --source mac-in-person` prints a meeting id;
    `export <id>` writes a `meeting.json` that decodes as `MeetingExport`.

## Tests

Unit (`Tests/StenoCoreTests`): `ModelCodableTests`, `MigrationsTests`,
`MeetingStoreTests`, `SearchTests`, `SettingsStoreTests`, `SecretStoreTests`,
`EventBusTests`, `RecordingIntakeTests`, `TemplateRegistryTests`,
`WAVAudioDecoderTests`, `SnapshotTests`, `LaneMergerTests`, one `*StepTests`
per pipeline step. Integration: `PipelineIntegrationTests` (in-memory store,
all fakes, `macCall` two-lane and `macInPerson` one-lane paths, rerun and
failure paths); `Tests/stenoTests/CLITests` (built binary, temp database,
fixture WAVs). No network anywhere; no model downloads.

Manual check (a human on a Mac): `swift run steno db migrate`, `steno process`
a fixture with `--audio-folder ~/Desktop/steno-test`, then in `sqlite3`
confirm `SELECT * FROM transcriptSegment_ft WHERE transcriptSegment_ft MATCH
'sweep'` returns rows and that `steno.sqlite-wal` exists while the CLI runs.

## Spikes

- S1, before step 3: FTS5 in GRDB's SPM build on the `macos-15` runner
  (GRDB's manifest defines `SQLITE_ENABLE_FTS5`; the FTS guide still says
  "custom SQLite build"). Covered by the step 1 acceptance test. Fallback:
  FTS4 in the same migration, identical API bar the module type.
- S2, before step 6: `Bundle.module` resolves for `steno` launched from
  `.build/debug` and from a copied binary. Fallback: templates as generated
  Swift string literals, noted in this plan.
- S3, before step 9: strict concurrency with `any PipelineStep` values in a
  `Sendable` struct and `inout PipelineContext` across `await`. Fallback:
  `func run(_ ctx: PipelineContext, deps:) async throws -> PipelineContext`.

## Needs from other workstreams

- StenoAudio: `AudioDecoding` (CAF, m4a, 48 kHz, mixdown), `EchoCanceller`,
  `AudioAsset` rows with `sidecars16k` via `MeetingStore.save(_:)`.
- StenoSpeech: two `SpeechEngine`s, `Diarizer` filling `sampleClipRange`,
  `SpeakerMemory` over `PersonStore`.
- StenoLLM: `LanguageModel`, `TranscriptCleaner`, `MeetingSummarizer`, prompt
  wording per `SummaryTemplate`.
- StenoAdapters: `DeliveryDispatcher`, `Destination` (Obsidian folder),
  renderers filling `MeetingExport.artifacts`.
- StenoHandover: calls `HandoverStore` and `HandoverIntake.admit`.
- macOS app: `SecretStore` on Keychain; consumes `observeMeetings()`,
  `observeMeeting(id:)`, `MeetingEventBus.subscribe()`, `SettingsStore.observe()`.

## Unverified API names

Every GRDB, swift-argument-parser and Foundation name above was checked
against GRDB 7.11.1 sources and DocC, swift-argument-parser 1.8.2 sources, or
Apple's `Locale.Language` page, except these, to confirm while implementing:
`Database.BusyMode.timeout(_:)` exact case name; `bm25()` ordering through the
query interface (fall back to raw SQL `ORDER BY bm25(transcriptSegment_ft)`);
the `swift format lint --strict --recursive` flag set on the Xcode 16.4
toolchain; `Bundle.module` for a copied executable (spike S2).

## Requested changes to the program document

Reconciled into the program document, see its log (entries 1 to 5).

## Deferred

- `SpeakerMemory.forget(personID:)` and person deletion: not in v1 scope;
  merging split speakers is covered by `merge`.
- Core's own sample-clip picker (`SampleClip.swift`): the diarizer chooses the
  clip range; core only copies it.
