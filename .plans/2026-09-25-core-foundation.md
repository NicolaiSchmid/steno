# Steno core foundation: package, StenoCore, `steno` CLI skeleton

Status: implementation plan, 2026-09-25. Workstream 1 of
[`2026-09-25-v1-program.md`](2026-09-25-v1-program.md); scope authority is
[`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md).

Protocol changes against the program document: nothing removed or renamed.
Added `SpeakerMemory.forget(personID:)` and four new protocols
(`AudioDecoding`, `TranscriptCleaner`, `MeetingSummarizer`, `SecretStore`);
see the final section.

## Goal

Create the Swift package every other workstream compiles against: the
canonical model, GRDB storage with one append-only migration file and FTS5,
the five pluggable protocols plus supporting types, a `ProcessingPipeline`
whose nine steps are injectable values, the template registry with the four
fixed templates as bundled resources, settings persistence, a meeting event
stream, and a `steno` CLI that can migrate a database, run the pipeline
against a WAV file with fakes, and export a meeting as JSON. When this merges,
audio, speech, LLM, adapters and handover can proceed in parallel against
compiled protocols and a green `macos-15` CI job.

## Non-goals

- Any real capture, STT, diarization, LLM or destination implementation.
  Fakes only, no network, no model downloads.
- Decoding compressed audio or resampling. Core reads 16 kHz mono PCM WAV;
  `AudioDecoding` for m4a/CAF/48 kHz belongs to StenoAudio.
- Markdown and VTT rendering (StenoAdapters); `steno export` writes JSON only.
  Keychain access (the macOS app owns the `SecretStore` implementation).
- SwiftUI, xcodegen, Sparkle, signing, the Forge runner, and any schema
  beyond the program's canonical model.

## Decisions

- `swift-tools-version: 6.1`, `platforms: [.macOS(.v15)]`,
  `swiftLanguageModes: [.v6]`. GRDB 7.11.1 requires Swift 6.1 / Xcode 16.3+;
  the `macos-15` runner defaults to Xcode 16.4 and offers 26.x.
- Dependencies declared now: GRDB.swift `from: "7.11.1"` and
  swift-argument-parser `from: "1.8.2"`. FluidAudio, WhisperKit and Sparkle
  are added by their own workstreams so this PR's CI does not pay for them.
- swift-argument-parser is the one extra package: nested subcommands
  (`db migrate`, `fixtures generate`), typed options, `--help` and
  `AsyncParsableCommand` for the async pipeline; hand-rolled parsing would be
  rewritten the first time the bake-off harness needs a flag.
- `.enableUpcomingFeature("InferSendableFromCaptures")` on every target, as
  GRDB documents for Swift 6 shorthand closures. Tests use Swift Testing
  (`@Test`, `#expect`) on an in-memory `DatabaseQueue()`; app and CLI use
  `DatabasePool(path:configuration:)` (WAL).
- Records are structs, `Codable + FetchableRecord + PersistableRecord`, hence
  `Sendable`. `MeetingStore` is a `Sendable` final class holding
  `let writer: any DatabaseWriter` (GRDB declares `DatabaseReader: AnyObject,
  Sendable`), not an actor: the pool already serialises writes and an actor
  would serialise reads too.
- Column encodings: `UUID` as uppercase TEXT (`databaseUUIDEncodingStrategy`
  `.uppercaseString`), `Date` as GRDB's default UTC text, `[String]` as
  `.jsonText`, embeddings as 1024-byte little-endian Float32 BLOB through an
  `Embedding: DatabaseValueConvertible` wrapper, `Locale.Language` as its
  BCP-47 identifier. Enums with payloads (`MeetingState.failed`,
  `AudioRetention.keepDays`, `DeliveryStatus`) flatten into two columns each
  in private row structs in `Storage/`; canonical types stay clean for
  `meeting.json`.
- Tables keep the implicit rowid (never `withoutRowID`) because FTS5 external
  content tables join on rowid. The app never runs `VACUUM`;
  `steno db reindex` rebuilds the FTS index if it drifts.
- One migration `"v1"` creates everything. `eraseDatabaseOnSchemaChange`
  stays `false` everywhere; append-only discipline instead.
- Templates are data (id, name, sections) in
  `Sources/StenoCore/Resources/Templates/*.json` behind `Bundle.module`.
  Prompt wording is StenoLLM's. Fakes live in `Sources/StenoCore/Fakes/` as
  public types so the CLI and every module's tests share them.

## Public API

Canonical model, all `Codable, Sendable, Equatable, Hashable, Identifiable`:

```swift
public struct Meeting {
    public var id: UUID; public var title: String; public var startedAt: Date
    public var duration: TimeInterval; public var language: Locale.Language?
    public var source: MeetingSource; public var calendarEventID: String?
    public var tags: [String]; public var state: MeetingState
    public var templateID: String; public var summary: String?
    public var scratchpad: String; public var createdAt: Date; public var updatedAt: Date
}
public enum MeetingSource: String { case macCall, macInPerson, phone };  public enum ParticipantRole: String { case me, them }
public enum AudioLane: String { case mic, system, mixed };               public enum TaskPriority: String { case low, normal, high }
public enum MeetingState { case recording, queued, processing, ready, failed(reason: String) }
public struct Participant { id, meetingID, personID: UUID?, displayName, role: ParticipantRole, email: String? }
public struct Person { id, displayName, email: String?, embedding: Embedding?, sampleCount: Int, createdAt }
public struct Embedding { public var values: [Float] /* 256, L2-normalised; BLOB via DatabaseValueConvertible */ }
public struct Speaker { id, meetingID, clusterLabel: String, personID: UUID?, embedding: Embedding?, sampleClipRange: ClosedRange<TimeInterval>, confidence: Float }
public struct TranscriptSegment { id, meetingID, start, end: TimeInterval, speakerID: UUID?, lane: AudioLane, text, rawText: String }
public struct MeetingTask { id, meetingID, text, assigneePersonID: UUID?, assigneeName: String?, priority: TaskPriority, dueDate: Date?, done: Bool }
public struct Decision { id, meetingID, text }
public struct AudioAsset { id, meetingID, url: URL, format: String, lanes: [AudioLane], durationSeconds: TimeInterval, retention: AudioRetention, expiresAt: Date? }
public enum AudioRetention { case deleteAfterProcessing, keepDays(Int), keepForever }
public struct Delivery { id, meetingID, destinationID: String, status: DeliveryStatus, lastAttemptAt: Date? }
public enum DeliveryStatus { case pending, delivered(Date), failed(String, Date) }
```

Protocols. `SpeechEngine`, `Diarizer`, `EchoCanceller`, `LanguageModel` and
`Destination` are copied verbatim from the program document. Additions:

```swift
public protocol SpeakerMemory: Sendable {
    func match(_ embedding: [Float]) async throws -> (Person, Float)?
    func enroll(_ embedding: [Float], as person: Person) async throws
    func forget(personID: UUID) async throws                                   // added: user merges or deletes a person
}
public protocol AudioDecoding: Sendable {                                      // new: pipeline step 1 boundary
    func decode(_ url: URL) async throws -> [AudioLane: AudioBuffer16k]
}
public protocol TranscriptCleaner: Sendable {                                  // new: pipeline step 6 boundary
    func clean(_ segments: [TranscriptSegment], language: Locale.Language?) async throws -> [TranscriptSegment]
}
public protocol MeetingSummarizer: Sendable {                                  // new: pipeline step 7 boundary
    func summarize(_ input: SummaryInput) async throws -> SummaryOutput
}
public protocol SecretStore: Sendable {                                        // new: API keys never in SQLite
    func secret(for key: SecretKey) async throws -> String?
    func setSecret(_ value: String?, for key: SecretKey) async throws
}
```

Supporting types:

```swift
public struct AudioBuffer16k: Sendable, Equatable {
    public static let sampleRate: Double = 16_000
    public var samples: [Float]; public var count: Int { samples.count }; public var duration: TimeInterval
}
public struct RawSegment: Codable, Sendable, Equatable { start, end: TimeInterval; text: String; language: Locale.Language?; wordTimings: [WordTiming]? }
public struct WordTiming: Codable, Sendable, Equatable { word: String; start, end: TimeInterval }
public struct DiarizationResult: Codable, Sendable, Equatable { public var clusters: [SpeakerCluster] }
public struct SpeakerCluster: Codable, Sendable, Equatable { label: String; ranges: [ClosedRange<TimeInterval>]; embedding: Embedding? }
public struct LLMRequest: Codable, Sendable { messages: [LLMMessage]; jsonSchema: JSONValue?; temperature: Double?; maxTokens: Int? }
public struct LLMMessage: Codable, Sendable { role: LLMRole; content: String };  public enum LLMRole: String { case system, user, assistant }
public struct LLMResponse: Codable, Sendable { text: String; promptTokens: Int?; completionTokens: Int?; model: String? }
public indirect enum JSONValue: Codable, Sendable, Equatable, Hashable { case string(String), number(Double), bool(Bool), null, array([JSONValue]), object([String: JSONValue]) }
public struct SummaryInput: Sendable { meeting: Meeting; segments: [TranscriptSegment]; speakers: [Speaker]; participants: [Participant]; calendarAttendees: [String]; template: SummaryTemplate }
public struct SummaryOutput: Codable, Sendable, Equatable { title: String; summaryMarkdown: String; decisions: [String]; tasks: [MeetingTask]; speakerNames: [UUID: String]; language: Locale.Language? }
public struct MeetingExport: Codable, Sendable, Equatable {
    meeting: Meeting; participants: [Participant]; speakers: [Speaker]; persons: [Person]; segments: [TranscriptSegment]
    tasks: [MeetingTask]; decisions: [Decision]; audio: AudioAsset?; renderedArtifacts: [RenderedArtifact]   // filled by StenoAdapters
}
public struct RenderedArtifact: Codable, Sendable, Equatable { fileName: String; contentType: String; data: Data }
public struct DestinationSettings: Codable, Sendable, Equatable { public subscript(key: String) -> JSONValue? }
public enum DeliveryMode: String, Codable, Sendable { case initial, reexport }
public struct SummaryTemplate: Identifiable { id: String; displayName: String; sections: [TemplateSection /* heading, instruction */] }
public struct SecretKey: RawRepresentable, Sendable, Hashable { public static let llmAPIKey: SecretKey }
public struct Settings: Codable, Sendable, Equatable {
    audioFolder: URL?; defaultRetention: AudioRetention; speechEngineID: String; llmBaseURL: URL?
    llmModel: String?; defaultTemplateID: String; launchAtLogin: Bool
    destinations: [String: DestinationSettings]; enabledDestinationIDs: Set<String>
}
public enum MeetingEvent: Sendable, Equatable {
    case stateChanged(meetingID: UUID, state: MeetingState)
    case meetingReady(meetingID: UUID)
    case speakersNeedReview(meetingID: UUID, speakerIDs: [UUID])
    case deliveryUpdated(Delivery)
}
```

Storage, pipeline, templates, settings, events:

```swift
public struct StenoPaths: Sendable { public static func `default`() throws -> StenoPaths; public var databaseURL: URL; public var supportDirectory: URL }
public enum Migrations { public static func migrator() -> DatabaseMigrator }             // one file, append-only
public final class MeetingStore: Sendable {
    public init(writer: any DatabaseWriter) throws                                       // runs migrator
    public static func onDisk(at url: URL) throws -> MeetingStore                        // DatabasePool, WAL, busyMode .timeout(5)
    public static func inMemory() throws -> MeetingStore                                 // DatabaseQueue()
    public func save(_ meeting: Meeting) async throws; public func meeting(id: UUID) async throws -> Meeting?
    public func meetings(limit: Int, offset: Int) async throws -> [Meeting]
    public func setState(_ state: MeetingState, meetingID: UUID) async throws
    public func replaceTranscript(meetingID: UUID, segments: [TranscriptSegment], speakers: [Speaker]) async throws
    public func replaceSummary(meetingID: UUID, output: SummaryOutput, templateID: String) async throws
    public func save(_ asset: AudioAsset) async throws; public func save(_ person: Person) async throws
    public func asset(id: UUID) async throws -> AudioAsset?; public func persons() async throws -> [Person]
    public func mergePersons(keep: UUID, remove: UUID) async throws
    public func upsert(_ delivery: Delivery) async throws; public func deliveries(meetingID: UUID) async throws -> [Delivery]
    public func export(meetingID: UUID) async throws -> MeetingExport
    public func search(_ query: String, limit: Int) async throws -> [SearchHit]           // FTS5, bm25 order
    public func observeMeetings() -> AsyncThrowingStream<[Meeting], Error>              // ValueObservation.values(in:)
    public func rebuildSearchIndex() async throws
    public func deleteExpiredAudio(now: Date) async throws -> [URL]                      // caller removes the files
}
public struct SearchHit: Sendable, Equatable { meetingID: UUID; segmentID: UUID?; snippet: String; rank: Double }
public final class SettingsStore: Sendable {                                              // key/value table `setting`
    public init(writer: any DatabaseWriter); public func load() async throws -> Settings
    public func save(_ settings: Settings) async throws; public func observe() -> AsyncThrowingStream<Settings, Error>
}
public actor MeetingEventBus { public func subscribe() -> AsyncStream<MeetingEvent>; public func post(_ event: MeetingEvent) }
public struct TemplateRegistry: Sendable {                                                // ids: default, customer-discovery, daily-standup, interview
    public static let bundled: TemplateRegistry; public static let defaultTemplateID = "default"   // Resources/Templates via Bundle.module
    public var templates: [SummaryTemplate]; public func template(id: String) -> SummaryTemplate?
}
public struct PipelineDependencies: Sendable {
    decoder: any AudioDecoding; speechEngine: any SpeechEngine; diarizer: any Diarizer; speakerMemory: any SpeakerMemory
    cleaner: any TranscriptCleaner; summarizer: any MeetingSummarizer; destinations: [any Destination]
    store: MeetingStore; settings: SettingsStore; events: MeetingEventBus; templates: TemplateRegistry
}
public struct PipelineSteps: Sendable {                                                   // `.default` wraps the dependencies; tests swap any step
    decode: any DecodeStep; transcribe: any TranscribeStep; diarize: any DiarizeStep; matchSpeakers: any MatchSpeakersStep
    merge: any MergeStep; cleanup: any CleanupStep; summarize: any SummarizeStep; persist: any PersistStep; deliver: any DeliverStep
}
public struct PipelineContext: Sendable { meeting: Meeting; asset: AudioAsset; lanes: [AudioLane: AudioBuffer16k]; raw: [AudioLane: [RawSegment]]; diarization: DiarizationResult?; speakers: [Speaker]; segments: [TranscriptSegment]; summary: SummaryOutput?; calendarAttendees: [String] }
public protocol DecodeStep: Sendable { func run(_ ctx: inout PipelineContext, deps: PipelineDependencies) async throws }   // same shape for the other eight
public actor ProcessingPipeline {
    public init(dependencies: PipelineDependencies, steps: PipelineSteps = .default)
    public func process(assetID: UUID) async throws                                      // steps 1-9; queued -> processing -> ready | failed
    public func rerunSummary(meetingID: UUID, templateID: String) async throws          // steps 7-9, .reexport
    public func redeliver(meetingID: UUID) async throws                                  // step 9, .reexport
}
// Fakes (Sources/StenoCore/Fakes): FakeSpeechEngine, FakeDiarizer, FakeLanguageModel, PassthroughCleaner, FakeSummarizer,
// InMemorySpeakerMemory, RecordingDestination, WAVAudioDecoder, FileSecretStore (CLI and tests only; 0600 file under supportDirectory)
```

`steno` CLI: `steno db migrate [--db PATH]`, `steno db reindex [--db PATH]`,
`steno process <wav> [--system-lane WAV] [--source mac-call|mac-in-person|phone] [--title T] [--template ID] [--db PATH] [--audio-folder DIR]`,
`steno export <meeting-id> [--out DIR] [--db PATH]` (writes `meeting.json`),
`steno fixtures generate --out DIR` (deterministic sine sweeps and two-tone
"conversations", 2 to 8 s, 16 kHz mono WAV). `--db` defaults to
`StenoPaths.default().databaseURL`
(`~/Library/Application Support/Steno/steno.sqlite`).

## Files

```
Package.swift                                   targets, products, deps, swift settings
Package.resolved                                committed
Sources/StenoCore/Model/{Meeting,People,Transcript,Outputs,Audio,Delivery,JSONValue}.swift
                                                one file per model group as listed in Public API
Sources/StenoCore/Model/Language+Codable.swift  Locale.Language <-> identifier helpers, DatabaseValueConvertible
Sources/StenoCore/Protocols/{SpeechEngine,EchoCanceller,LanguageModel,Destination,SpeakerMemory}.swift
                                                program protocols plus their request/response types
Sources/StenoCore/Protocols/PipelineBoundaries.swift  AudioDecoding, TranscriptCleaner, MeetingSummarizer, SummaryInput, SummaryOutput
Sources/StenoCore/Protocols/SecretStore.swift   SecretStore, SecretKey
Sources/StenoCore/Storage/Migrations.swift      the only migration file; "v1" registers tables, indexes, FTS5
Sources/StenoCore/Storage/Records.swift         private row structs for payload enums, Columns enums, encoding strategies
Sources/StenoCore/Storage/MeetingStore{,+Search,+Export}.swift  store, FTS5 queries and reindex, export(meetingID:)
Sources/StenoCore/Storage/{SettingsStore,StenoPaths}.swift      settings key/value table, on-disk locations
Sources/StenoCore/Pipeline/{ProcessingPipeline,PipelineContext}.swift  actor and state machine; context, dependencies, steps
Sources/StenoCore/Pipeline/Steps/*.swift        one file per step (Decode, Transcribe, Diarize, MatchSpeakers, Merge, Cleanup, Summarize, Persist, Deliver)
Sources/StenoCore/Pipeline/{LaneMerger,SampleClip}.swift  pure functions: lane merge with "me" participant; 10 s clip per unmatched cluster
Sources/StenoCore/Events/MeetingEventBus.swift  MeetingEvent, MeetingEventBus
Sources/StenoCore/Templates/TemplateRegistry.swift   SummaryTemplate, TemplateSection, TemplateRegistry
Sources/StenoCore/Resources/Templates/{default,customer-discovery,daily-standup,interview}.json
Sources/StenoCore/Fakes/*.swift                 FakeSpeechEngine, FakeDiarizer, FakeLanguageModel, PassthroughCleaner, FakeSummarizer, InMemorySpeakerMemory, RecordingDestination, FileSecretStore
Sources/StenoCore/Audio/WAVAudioDecoder.swift   RIFF/PCM reader, 16 kHz mono only, 16-bit int and 32-bit float
Sources/StenoCore/Audio/WAVWriter.swift         writes 16 kHz mono 16-bit WAV (fixtures, sample clips later)
Sources/StenoAudio/StenoAudio.swift             placeholder `public enum StenoAudio {}` (same for StenoSpeech, StenoLLM, StenoAdapters, StenoHandover)
Sources/steno/Steno.swift                       @main AsyncParsableCommand, subcommands
Sources/steno/{Commands/{DB,Process,Export,Fixtures},Wiring}.swift  subcommands; PipelineDependencies from fakes and flags
Tests/StenoCoreTests/*.swift                    see Tests
Tests/StenoAudioTests/PlaceholderTests.swift    one passing test per placeholder module (same for the other four)
Tests/stenoTests/CLITests.swift                 runs the built binary via Process against a temp db
Tests/Fixtures/README.md                        convention: paths, sizes, how to regenerate
Tests/Fixtures/Audio/*.wav                      generated by `steno fixtures generate`, each under 10 s
Tests/Fixtures/Transcripts/*.json               [RawSegment] and [TranscriptSegment] samples (invented text)
Tests/Fixtures/Templates/*.json                 expected registry output, snapshot
Tests/Fixtures/Exports/meeting-export.json      MeetingExport snapshot for step 2
.github/workflows/swift-ci.yml                  see step 1
```

Fixture access: each test target has `Fixtures.url(_ relative: String) -> URL`
resolving from `#filePath` to `Tests/Fixtures/` (SwiftPM resources cannot point outside the target directory).

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
   `Tests/Fixtures/Exports/meeting-export.json` byte for byte (sorted keys).
3. **Migration v1 and records (1 d).** Tables `meeting`, `participant`,
   `person`, `speaker`, `transcriptSegment`, `meetingTask`, `decision`,
   `audioAsset`, `delivery`, `setting`; foreign keys to `meeting(id)` with
   `onDelete: .cascade`; indexes on `(meetingID, start)` and `state`; FTS5
   `transcriptSegment_ft(text)` and `meeting_ft(title, summary)` via
   `t.synchronize(withTable:)`, `t.tokenizer = .unicode61()`. Acceptance:
   migrator runs on an empty in-memory queue; `PRAGMA foreign_key_check`
   empty; an inserted segment is found via `FTS5Pattern(matchingAllTokensIn:)`;
   `appliedIdentifiers` equals `["v1"]` so later changes must append.
4. **MeetingStore CRUD, export, search, observation (1 d).** Acceptance:
   unit tests per method; `observeMeetings()` yields a second value after a
   `save`; `search("Jérôme")` matches "jerome" through unicode61;
   `mergePersons` re-points speakers and participants, deletes the loser.
5. **Settings, secrets, paths, events (0.5 d).** Acceptance: default
   `Settings` has `defaultTemplateID == "default"` and `defaultRetention ==
   .keepDays(30)`; save then load round-trips; `FileSecretStore` writes mode
   0600; `MeetingEventBus` fans one post out to two subscribers.
6. **Template registry and resources (0.5 d).** Four JSON templates with
   section headings modelled on Jamie's. Acceptance:
   `TemplateRegistry.bundled.templates.map(\.id)` equals the four ids in
   order; `Bundle.module` loads under `swift test` and from the built binary.
7. **Fakes and WAV decoder (0.5 d).** Fakes deterministic and configurable
   (segments per lane, cluster count, canned `SummaryOutput`). Acceptance:
   decoding `Tests/Fixtures/Audio/sweep-3s.wav` yields 48 000 samples; a
   48 kHz or stereo file throws `WAVDecodeError.unsupportedFormat`.
8. **Pipeline steps 1 to 5 (1 d).** Decode, transcribe per lane, diarize the
   right lane by `MeetingSource`, match against `SpeakerMemory` with a 0.72
   cosine threshold (constant, tunable by StenoSpeech), lane merge with
   deterministic "me" participant, sample clip selection (longest contiguous
   range, capped at 10 s). Acceptance: unit tests per step with fakes;
   `LaneMerger` property test: output sorted by `start`, no segment lost.
9. **Pipeline steps 6 to 9, state machine, reruns (1 d).** Failures in 6 or 7
   keep the transcript and set `.failed(reason:)`; `rerunSummary` runs 7 to 9
   with `.reexport`; `redeliver` runs 9. Events posted on each transition.
   Acceptance: integration test drives `process(assetID:)` end to end on an
   in-memory store with all fakes and asserts the final `MeetingExport`, the
   `RecordingDestination` call with `.initial`, and the event sequence
   `queued -> processing -> ready, meetingReady`; a test with a throwing
   `FakeSummarizer` asserts `.failed` plus intact segments.
10. **CLI (1 d).** Subcommands as listed, `Wiring.swift`, exit codes (0 ok,
    1 usage, 2 runtime). Acceptance: `Tests/stenoTests` runs the binary:
    `db migrate` on a temp path creates the file; `fixtures generate` then
    `process sweep-3s.wav --source mac-in-person` prints a meeting id;
    `export <id>` writes a `meeting.json` that decodes as `MeetingExport`.

## Tests

Unit (`Tests/StenoCoreTests`): `ModelCodableTests`, `MigrationsTests`,
`MeetingStoreTests`, `SearchTests`, `SettingsStoreTests`, `SecretStoreTests`,
`EventBusTests`, `TemplateRegistryTests`, `WAVAudioDecoderTests`,
`LaneMergerTests`, `SampleClipTests`, one `*StepTests` per pipeline step.
Integration: `PipelineIntegrationTests` (in-memory store, all fakes, `macCall`
two-lane and `macInPerson` one-lane paths, rerun and failure paths);
`Tests/stenoTests/CLITests` (built binary, temp database, fixture WAVs). No
network anywhere; no model downloads.

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
- S3, before step 9: strict concurrency with `any DecodeStep` values in a
  `Sendable` struct and `inout PipelineContext` across `await`. Fallback:
  `func run(_ ctx: PipelineContext, deps:) async throws -> PipelineContext`.

## Needs from other workstreams

- StenoAudio: `AudioDecoding` for m4a/CAF/48 kHz (AVFoundation),
  `EchoCanceller`, `AudioAsset` rows via `MeetingStore.save(_:)`.
- StenoSpeech: two `SpeechEngine`s, `Diarizer`, `SpeakerMemory` over
  `MeetingStore.persons()` / `save(_ person:)`.
- StenoLLM: `LanguageModel`, `TranscriptCleaner`, `MeetingSummarizer`, prompt
  text per `SummaryTemplate`.
- StenoAdapters: `Destination` (Obsidian folder) and renderers filling
  `MeetingExport.renderedArtifacts`.
- StenoHandover: creates a `.phone` `Meeting` plus `AudioAsset`, then calls
  `ProcessingPipeline.process(assetID:)`.
- macOS app: `SecretStore` on Keychain; consumes `observeMeetings()`,
  `MeetingEventBus.subscribe()`, `SettingsStore.observe()`.

## Unverified API names

Every GRDB, swift-argument-parser and Foundation name above was checked
against GRDB 7.11.1 sources and DocC, swift-argument-parser 1.8.2 sources, or
Apple's `Locale.Language` page, except these, to confirm while implementing:
`Database.BusyMode.timeout(_:)` exact case name; `bm25()` ordering through the
query interface (fall back to raw SQL `ORDER BY bm25(transcriptSegment_ft)`);
the `swift format lint --strict --recursive` flag set on the Xcode 16.4
toolchain; `Bundle.module` for a copied executable (spike S2).

## Requested changes to the program document

1. Add `AudioDecoding`, `TranscriptCleaner`, `MeetingSummarizer` and
   `SecretStore` to the protocols owned by StenoCore (implemented by
   StenoAudio, StenoLLM, StenoLLM, macOS app). Without them pipeline steps
   1, 6 and 7 would need StenoLLM prompt code and AVFoundation inside
   StenoCore, contradicting "depends on nothing but GRDB and Foundation".
2. Add `forget(personID:)` to `SpeakerMemory` (user merges split speakers).
3. Record that template structure (ids, section headings, instructions) is
   StenoCore data under `Resources/Templates/` and StenoLLM owns only prompt
   wording; the workstream table lists "four templates" under StenoLLM.
4. State that StenoCore decodes 16 kHz mono WAV only; other formats and
   resampling go through StenoAudio's `AudioDecoding`.
5. Add `Tests/stenoTests` to the layout and the `#filePath` fixture-access
   convention. Note that FluidAudio, WhisperKit and Sparkle enter
   `Package.swift` in their own workstream PRs, not in core foundation.
