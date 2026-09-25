# Steno core foundation: package, StenoCore, `steno` CLI skeleton

Status: implementation plan, 2026-09-25, reconciled and then revised the same day after the three
reviews (see the program's review application log). Workstream 1 of
[`2026-09-25-v1-program.md`](2026-09-25-v1-program.md); scope authority is
[`2026-09-24-initial-scope.md`](2026-09-24-initial-scope.md). Model, protocol and supporting-type
shapes below are the program's; this plan adds storage, pipeline and CLI detail only.

## Goal

Create the Swift package every other workstream compiles against: the canonical model, GRDB storage
with one append-only migration file and FTS5, the program's protocols plus supporting types, a
`ProcessingPipeline` actor whose stages are private typed functions, the summary Markdown renderer,
the four fixed templates as bundled JSON, settings persistence, the meeting event bus, the retention
sweep, the shared test support (`Testing/`: fakes, `ManualClock`, fixture access, golden snapshots),
the `StenoEndToEndTests` target, and a `steno` CLI that can migrate a database, run the pipeline
against a WAV file with fakes, and export a meeting as JSON. When this merges, audio, speech, LLM,
adapters and handover proceed in parallel against compiled protocols and a green `macos-15` CI job
that reports test counts and skips.

## Non-goals

- Any real capture, STT, diarization, LLM or destination implementation. Fakes only, no network, no
  model downloads.
- Decoding compressed audio, resampling or AAC mixdown. Core's `WAVAudioDecoder` reads 16 kHz mono
  PCM WAV for fixtures and the CLI; the real `AudioDecoder` is StenoAudio's.
- Vault rendering and delivery (`2026-09-25-adapters-obsidian.md`); `steno export` writes
  `meeting.json` only. Keychain access (the macOS app owns the `SecretStore` implementation).
- SwiftUI, xcodegen, Sparkle, signing, the Forge runner, and any schema beyond the program's model.

## Decisions

- `swift-tools-version: 6.1`, `platforms: [.macOS(.v15)]`, `swiftLanguageModes: [.v6]`. GRDB 7.11.1
  (released 2026-06-18) requires Swift 6.1 / Xcode 16.3+; the `macos-15` runner image defaults to
  Xcode 16.4 and offers 26.x (verified: actions/runner-images macos-15 readme).
- Dependencies declared now: GRDB.swift `from: "7.11.1"` and swift-argument-parser `from: "1.8.2"`
  (released 2026-06-04). FluidAudio, WhisperKit, CSpeex, swift-certificates, swift-nio and Sparkle
  are added by their own workstreams.
- swift-argument-parser: nested subcommands (`dev db migrate`, `dev fixtures generate`), typed
  options, `--help` and `AsyncParsableCommand`. Product commands are `record`, `process`, `export`,
  `deliver`; every developer tool sits under `steno dev`.
- `.enableUpcomingFeature("InferSendableFromCaptures")` on every target, as GRDB documents for
  Swift 6 shorthand closures. Tests use Swift Testing (`@Test`, `#expect`) on an in-memory
  `DatabaseQueue()`; app and CLI use `DatabasePool(path:configuration:)` (WAL).
- Records are structs, `Codable + FetchableRecord + PersistableRecord`, hence `Sendable`.
  `MeetingStore` is a `Sendable` final class holding `let writer: any DatabaseWriter`, not an actor:
  the pool already serialises writes and an actor would serialise reads too. It is passed directly
  wherever speech or handover need people or handover rows; there is no `PersonStore` or
  `HandoverStore` protocol (one implementation).
- One JSON convention, `StenoJSON.encoder()` / `decoder()`: camelCase, `.sortedKeys`, ISO 8601 with
  fractional seconds (custom strategy), `Data` as standard base64. Used for `meeting.json`, JSON
  columns and the handover wire. `Person.embedding` and `Speaker.embedding` have no CodingKey; the
  row types persist them as BLOBs.
- Column encodings: `UUID` as uppercase TEXT, `Date` as GRDB's default UTC text, `[String]` as
  `.jsonText`, embeddings as 1024-byte little-endian Float32 BLOB through `Embedding:
  DatabaseValueConvertible`, `Locale.Language` as its BCP-47 identifier, `DeliveryReceipt`,
  `LLMUsage` and `SummaryDocument` as JSON text; `meeting.summaryText` holds the concatenated bullet
  text for FTS. Enums with payloads (`MeetingState.failed`, `AudioRetention.keepDays`,
  `DeliveryStatus.failed`, `SpeakerAssignment`, `HandoverReceipt.state`) flatten into columns in
  private row structs in `Storage/`; canonical types stay clean for `meeting.json`.
- Tables keep the implicit rowid (never `withoutRowID`) because FTS5 external content tables join on
  rowid. The app never runs `VACUUM`; `steno dev db reindex` rebuilds the FTS index if it drifts.
- One migration `"v1"` creates everything. `eraseDatabaseOnSchemaChange` stays `false` everywhere;
  append-only discipline, guarded by `SchemaSnapshotTests`.
- FTS5 is a go without a spike: GRDB's `Package.swift` always defines `SQLITE_ENABLE_FTS5` and links
  the system SQLite through the `GRDBSQLite` system-library target; Apple's libsqlite3 ships FTS5
  (verified: GRDB `Package.swift`, `Documentation/FullTextSearch.md`, whose "custom SQLite build"
  sentence is stale for Apple platforms). Rank ordering is `.order(Column.rank)`;
  `Database.BusyMode.timeout(_:)`, `synchronize(withTable:)` and
  `FTS5Pattern(matchingAllTokensIn:)` exist as used (verified: GRDB sources).
- Templates are data (id, displayName, description, context, sections with id, heading,
  instructions, required) in `Sources/StenoCore/Resources/Templates/*.json` behind `Bundle.module`,
  exposed as `SummaryTemplate.bundled`. Section ids and headings are fixed here; the `instructions`
  and `context` strings are prompt text that the LLM workstream edits in PRs against this module.
- The pipeline is one actor with private typed stage functions and a `PipelineStage` enum; no step
  protocol, no context of optionals. `PipelineDependencies` is the only injection axis. One
  failure type, `PipelineFailure`, and one catch in `process` marks the meeting `.failed`.
- `Sources/StenoCore/Testing/` holds every fake of a core protocol, `ManualClock`, `Fixtures.url(_:)`
  and `Snapshot`, public, so all test targets and the CLI share them. Modules add
  `Sources/<Module>/Testing/` for their own seams. Fixture folders under `Tests/Fixtures/` are
  lowercase because APFS is case-insensitive by default. Every test that writes files uses a fresh
  temp directory; wall time enters through `now: @Sendable () -> Date` or `any Clock<Duration>`.
- `Tests/StenoEndToEndTests` is created here with fakes everywhere and grows a real module per
  workstream; it never downloads models or opens a non-loopback socket.

## Public API

Canonical model exactly as listed in the program (`Meeting`, `SummaryDocument`, `SummarySection`,
`SummaryBullet`, `Participant`, `Person`, `Embedding`, `Speaker`, `SpeakerAssignment`,
`TranscriptSegment`, `MeetingTask`, `Decision`, `AudioAsset` with `AudioFormat`, `AudioLane`,
`AudioRetention`, `Delivery`, `DeliveryStatus`, `DeliveryReceipt`, `PairedDevice`,
`HandoverReceipt`, `LLMUsage`, `Settings`, `ObsidianSettings`), all `Codable, Sendable, Equatable,
Hashable`, row types `Identifiable`. Protocols exactly as in the program: `SpeechEngine`,
`Diarizer`, `EchoCanceller`, `LanguageModel`, `Destination`, `SpeakerMemory` (with the provided
`match`), `AudioDecoder`, `TranscriptCleaner`, `MeetingSummarizer`, `DeliveryDispatcher`,
`SecretStore`, `HandoverIntake`. Supporting types as in the program; shapes that need spelling out:

```swift
public struct AudioBuffer16k: Sendable, Equatable { public static let sampleRate: Double = 16_000; public var samples: [Float]; public var duration: TimeInterval }
public struct RawSegment: Codable, Sendable, Equatable { start, end: TimeInterval; text: String; language: Locale.Language?; wordTimings: [WordTiming]? }
public struct WordTiming: Codable, Sendable, Equatable { word: String; start, end: TimeInterval }
public struct DiarizationResult: Codable, Sendable, Equatable { public var clusters: [SpeakerCluster] }
public struct SpeakerCluster: Codable, Sendable, Equatable { label: String; ranges: [ClosedRange<TimeInterval>]; embedding: Embedding?; clusterConfidence: Float; sampleClipRange: ClosedRange<TimeInterval>? }
public enum SpeakerAssignment: Codable, Sendable, Equatable, Hashable { case unknown, suggested(personID: UUID, similarity: Float), confirmed(personID: UUID) }
public struct SpeakerMatch: Sendable, Equatable { public var person: Person; public var similarity: Float }
public struct LLMRequest: Codable, Sendable { messages: [LLMMessage]; responseFormat: LLMResponseFormat; temperature: Double?; maxTokens: Int?; purpose: String }
public enum LLMResponseFormat: Codable, Sendable, Equatable { case text, jsonObject, jsonSchema(name: String, schema: JSONValue, strict: Bool) }
public struct LLMMessage: Codable, Sendable { role: LLMRole; content: String };  public enum LLMRole: String { case system, user, assistant }
public struct LLMResponse: Codable, Sendable { text: String; finishReason: LLMFinishReason; usage: LLMUsage?; model: String? }
public enum LLMFinishReason: String, Codable, Sendable { case stop, length, contentFilter, other }
public indirect enum JSONValue: Codable, Sendable, Equatable, Hashable { case string(String), number(Double), bool(Bool), null, array([JSONValue]), object([String: JSONValue]) }
public struct CleanupInput: Sendable { segments: [TranscriptSegment]; language: Locale.Language?; participants: [Participant]; speakers: [Speaker]; knownPeople: [Person] }
public struct CleanupOutput: Sendable, Equatable { segments: [TranscriptSegment]; failedChunks: [Int]; usage: LLMUsage }
public struct SummaryInput: Sendable { meeting: Meeting; segments: [TranscriptSegment]; speakers: [Speaker]; participants: [Participant]; knownPeople: [Person]; template: SummaryTemplate }
public struct SummaryOutput: Codable, Sendable, Equatable { title: String; summary: SummaryDocument; decisions: [String]; tasks: [MeetingTask]; speakerNames: [SpeakerNameSuggestion]; language: Locale.Language?; usage: LLMUsage }
public struct SpeakerNameSuggestion: Codable, Sendable, Equatable { speakerID: UUID; name: String?; confidence: Double; evidence: String }
public struct MeetingExport: Codable, Sendable, Equatable {                      // its StenoJSON encoding is meeting.json everywhere
    public static let currentSchemaVersion = 1
    schemaVersion: Int; meeting: Meeting; participants: [Participant]; speakers: [Speaker]; persons: [Person]; segments: [TranscriptSegment]
    tasks: [MeetingTask]; decisions: [Decision]; audio: AudioAsset?
    public func displayName(forSpeaker id: UUID) -> String                        // confirmed or suggested person, else clusterLabel
}
public enum StenoJSON { public static func encoder() -> JSONEncoder; public static func decoder() -> JSONDecoder }   // camelCase, sortedKeys, ISO 8601 fractional
public enum SummaryMarkdown { public static func render(_ export: MeetingExport) -> String }   // "## heading" per section, "- **lead**: text", labels -> current names in bold
public enum CalendarMatch { public static func pick(events: [(id: String, start: Date, end: Date)], now: Date, lookahead: TimeInterval = 900) -> String? }
public struct RecordingMetadata: Codable, Sendable, Equatable { recordingID: UUID; startedAt: Date; durationSeconds: Double; byteCount: Int64; sha256: Data; chunkSize: Int; format: AudioFormat; deviceName: String }
public struct SummaryTemplate: Codable, Sendable, Identifiable { id: String; displayName: String; description: String; context: String; sections: [TemplateSection]
    public static let bundled: [SummaryTemplate]; public static let defaultID = "default"; public static func bundled(id: String) -> SummaryTemplate? }
public struct TemplateSection: Codable, Sendable, Equatable { id: String; heading: String; instructions: String; required: Bool }
public struct SecretKey: RawRepresentable, Sendable, Hashable { public static let llmAPIKey: SecretKey }
public enum PipelineStage: String, CaseIterable, Sendable, Codable { case decode, transcribe, diarize, matchSpeakers, merge, cleanup, summarize, persist, deliver, retention }
public struct PipelineFailure: Error, Sendable, Equatable { public var stage: PipelineStage; public var reason: String }
public enum MeetingEvent: Sendable, Equatable {
    case progress(meetingID: UUID, stage: PipelineStage, fraction: Double)      // one per stage start
    case speakersNeedReview(meetingID: UUID, speakerIDs: [UUID])               // speakers not .confirmed after persist
}
```

Storage, pipeline, templates, settings, events, intake, sweep:

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
    public func replaceSummary(meetingID: UUID, output: SummaryOutput, templateID: String) async throws   // writes summary JSON and summaryText
    public func save(_ asset: AudioAsset) async throws; public func asset(id: UUID) async throws -> AudioAsset?
    public func persons() async throws -> [Person]; public func save(_ person: Person) async throws
    public func mergePersons(keep: UUID, remove: UUID) async throws                      // re-points speakers and participants; sample-count-weighted mean, renormalised
    public func mergeSpeakers(_ source: UUID, into target: UUID, meetingID: UUID) async throws   // in-meeting cluster merge
    public func confirm(speakerID: UUID, person: Person, memory: any SpeakerMemory) async throws  // saves person if new, .confirmed, enroll, deletes sampleClipURL
    public func save(_ delivery: Delivery) async throws; public func deliveries(meetingID: UUID) async throws -> [Delivery]
    public func export(meetingID: UUID) async throws -> MeetingExport
    public func search(_ query: String, limit: Int) async throws -> [SearchHit]           // FTS5, .order(Column.rank)
    public func observeMeetings() -> AsyncThrowingStream<[Meeting], Error>              // ValueObservation.values(in:)
    public func observeMeeting(id: UUID) -> AsyncThrowingStream<MeetingExport?, Error>
    public func observeDeliveries(meetingID: UUID) -> AsyncThrowingStream<[Delivery], Error>
    public func rebuildSearchIndex() async throws
    public func expiredAssets(now: Date) async throws -> [AudioAsset]
    // handover rows: pairedDevices(), save(_ device:, tokenHash:), device(forTokenHash:), delete(deviceID:), receipt(_ recordingID:), save(_ receipt:)
}
public struct SearchHit: Sendable, Equatable { meetingID: UUID; segmentID: UUID?; snippet: String; rank: Double }
public struct RetentionSweep: Sendable {                                                  // removes master, sidecars, mixdown; never sample clips
    public init(store: MeetingStore, fileManager: FileManager = .default)
    public func run(now: Date) async throws -> [URL]                                     // clears expiresAt, continues past missing files
}
public final class SettingsStore: Sendable {                                              // key/value table `setting`
    public init(writer: any DatabaseWriter); public func load() async throws -> Settings
    public func save(_ settings: Settings) async throws; public func observe() -> AsyncThrowingStream<Settings, Error>
}
public actor MeetingEventBus { public func subscribe() -> AsyncStream<MeetingEvent>; public func post(_ event: MeetingEvent) }
public struct PipelineDependencies: Sendable {
    decoder: any AudioDecoder; speechEngine: any SpeechEngine; diarizer: any Diarizer; speakerMemory: any SpeakerMemory
    cleaner: any TranscriptCleaner; summarizer: any MeetingSummarizer; delivery: any DeliveryDispatcher
    store: MeetingStore; settings: SettingsStore; events: MeetingEventBus; now: @Sendable () -> Date = Date.init
}
public actor ProcessingPipeline {
    public init(dependencies: PipelineDependencies)
    public func enqueue(_ meeting: Meeting, asset: AudioAsset) async throws           // Meeting(.queued) + asset in one transaction, then Task { process }
    public func process(assetID: UUID) async throws                                      // all stages; queued -> processing -> ready | failed
    public func rerunSummary(meetingID: UUID, templateID: String) async throws          // summarize -> deliver
    public func redeliver(meetingID: UUID) async throws                                  // deliver only; the one re-export entry point
    // private: decodeAndTranscribe, diarize, matchSpeakers, mergeLanes, cleanup, summarize, persist, deliver, retention
}
public struct RecordingIntake: HandoverIntake, Sendable {                                  // phone recordings
    public init(store: MeetingStore, settings: SettingsStore, pipeline: ProcessingPipeline)
    public func admit(file: URL, metadata: RecordingMetadata, device: PairedDevice) async throws -> UUID
    // moves the file into audioFolder/<meetingID>/, then pipeline.enqueue(Meeting(.queued, .phone, title from startedAt),
    // AudioAsset(.m4aAAC, [.mixed], mixdownURL = url, retention from Settings)); idempotent on recordingID via HandoverReceipt
}
// Testing/: FakeSpeechEngine, FakeDiarizer, FakeLanguageModel, PassthroughCleaner, FakeSummarizer, InMemorySpeakerMemory, RecordingDestination,
// RecordingDispatcher, FakeHandoverIntake, WAVAudioDecoder (16 kHz WAV only; mixdown copies the file), FileSecretStore (0600 file),
// ManualClock: Clock<Duration> (advance(by:) fires sleepers deterministically), Fixtures.url(_ relative: String) -> URL,
// Snapshot.assert(_ data: Data, matches relative: String) (STENO_UPDATE_SNAPSHOTS=1 rewrites)
```

`steno` CLI: `steno process <wav> [--system-lane WAV] [--source mac-call|mac-in-person|phone] [--title T]
[--template ID] [--db PATH] [--audio-folder DIR]`, `steno export <meeting-id> [--out DIR] [--db PATH]`
(writes `meeting.json` through `StenoJSON`), `steno dev db migrate|reindex [--db PATH]`, `steno dev
fixtures generate --out DIR` (seeded, deterministic sine sweeps and two-tone "conversations", 2 to
8 s, 16 kHz mono Int16 WAV; other workstreams add cases). `--db` defaults to
`StenoPaths.default().databaseURL` (`~/Library/Application Support/Steno/steno.sqlite`).
`Wiring.swift` builds `PipelineDependencies` from fakes; later workstreams swap real implementations
in behind flags in their own command files, with one exception recorded here: the speech PR adds
`--engine <id>` to `Wiring.swift` so `steno process` can run the real engines.

## Files

```
Package.swift                                   targets, products, deps, swift settings
Package.resolved                                committed
Sources/StenoCore/Model/{Meeting,Summary,People,Transcript,Outputs,Audio,Delivery,Handover,Settings,Calendar,JSONValue,StenoJSON}.swift
Sources/StenoCore/Model/Language+Codable.swift  Locale.Language <-> identifier helpers, DatabaseValueConvertible
Sources/StenoCore/Protocols/{SpeechEngine,Diarizer,EchoCanceller,LanguageModel,Destination,SpeakerMemory,PipelineBoundaries,SecretStore}.swift
                                                PipelineBoundaries = AudioDecoder, TranscriptCleaner, MeetingSummarizer, DeliveryDispatcher, HandoverIntake
Sources/StenoCore/Storage/Migrations.swift      the only migration file; "v1" registers tables, indexes, FTS5
Sources/StenoCore/Storage/Records.swift         private row structs for payload enums, Columns enums, encoding strategies
Sources/StenoCore/Storage/MeetingStore{,+Search,+Export,+People,+Handover}.swift  store, FTS5, export(meetingID:), people and merges, handover rows
Sources/StenoCore/Storage/{SettingsStore,StenoPaths,RecordingIntake,RetentionSweep}.swift
Sources/StenoCore/Pipeline/ProcessingPipeline.swift   actor, state machine, the single catch that marks .failed
Sources/StenoCore/Pipeline/Stages/*.swift       one private extension per stage function (DecodeTranscribe, Diarize, MatchSpeakers, Merge, Cleanup, Summarize, Persist, Deliver, Retention)
Sources/StenoCore/Pipeline/LaneMerger.swift     pure function: lane merge with "me" participant
Sources/StenoCore/Summary/SummaryMarkdown.swift pure renderer: SummaryDocument + current names -> Markdown
Sources/StenoCore/Events/MeetingEventBus.swift  MeetingEvent, MeetingEventBus
Sources/StenoCore/Templates/SummaryTemplate.swift   SummaryTemplate, TemplateSection, bundled loader
Sources/StenoCore/Resources/Templates/{default,customer-discovery,daily-standup,interview}.json
Sources/StenoCore/Testing/*.swift               fakes, ManualClock, Fixtures, Snapshot (see Public API)
Sources/StenoCore/Audio/WAVAudioDecoder.swift   RIFF/PCM reader, 16 kHz mono only, 16-bit int and 32-bit float; conforms to AudioDecoder
Sources/StenoCore/Audio/WAVWriter.swift         writes 16 kHz mono 16-bit WAV (fixtures, sample clips)
Sources/StenoAudio/StenoAudio.swift             placeholder `public enum StenoAudio {}` (same for StenoSpeech, StenoLLM, StenoAdapters, StenoHandover)
Sources/steno/Steno.swift                       @main AsyncParsableCommand; `dev` group
Sources/steno/{Commands/{Process,Export,Dev,DevDB,DevFixtures},Wiring}.swift  subcommands; PipelineDependencies from fakes and flags
Tests/StenoCoreTests/*.swift                    see Tests
Tests/StenoAudioTests/PlaceholderTests.swift    one passing test per placeholder module (same for the other four)
Tests/stenoTests/CLITests.swift                 runs the built binary via Process against a temp db and temp HOME
Tests/StenoEndToEndTests/EndToEndTests.swift    real pipeline across modules, fakes replaced per workstream
Tests/Fixtures/README.md                        convention: lowercase folders, sizes, how to regenerate, MANIFEST rule
Tests/Fixtures/MANIFEST.sha256                  hashes of every generated fixture
Tests/Fixtures/audio/*.wav                      generated by `steno dev fixtures generate`, each under 10 s, incl. conversation-two-lane-6s.wav
Tests/Fixtures/transcripts/*.json               [RawSegment] and [TranscriptSegment] samples (invented text)
Tests/Fixtures/templates/*.json                 expected bundled output, snapshot
Tests/Fixtures/exports/meeting-export.json      MeetingExport snapshot for step 2
Tests/Fixtures/snapshots/schema/v1.sql          sqlite_master dump after migration v1
.github/workflows/swift-ci.yml                  see step 1
```

## Steps

1. **Package skeleton and CI (0.5 d).** `Package.swift` with seven targets, eight test targets
   (including an empty `StenoEndToEndTests`), placeholders, GRDB and ArgumentParser. `swift-ci.yml`:
   `actions/cache` on `.build` keyed by `Package.resolved` and `swift --version`, `swift format
   lint --strict --recursive Sources Tests Package.swift` (flags verified: swift-format README;
   `swift format` is the toolchain entry point from Xcode 16), `swift test --parallel
   --xunit-output .build/junit.xml`, a step that writes totals and every skipped test's name and
   message to `$GITHUB_STEP_SUMMARY` and fails when a skip message names no `STENO_*` switch,
   `timeout-minutes: 60`. Acceptance `[ci]`: CI green with the summary showing eight placeholder
   tests and zero skips; a test executing `CREATE VIRTUAL TABLE t USING fts5(x)` on
   `DatabaseQueue()` passes.
2. **Canonical model and supporting types (1 d).** All types in `Model/` and `Protocols/`,
   `JSONValue`, `StenoJSON`, `Locale.Language` helpers. Acceptance `[ci]`: a round-trip test encodes
   every model type through `StenoJSON` and back with equality (embeddings excluded by design);
   `MeetingExport` for a synthetic meeting with sequential UUIDs and fixed dates matches
   `Tests/Fixtures/exports/meeting-export.json` byte for byte; two encodes are byte-identical; no
   `embedding` key appears.
3. **Migration v1 and records (1 d).** Tables `meeting`, `participant`, `person`, `speaker`,
   `transcriptSegment`, `meetingTask`, `decision`, `audioAsset`, `delivery`, `pairedDevice`,
   `handoverReceipt`, `setting`; foreign keys to `meeting(id)` with `onDelete: .cascade`; indexes on
   `(meetingID, start)`, `state`, `audioAsset.expiresAt`, `pairedDevice.tokenHash`; FTS5
   `transcriptSegment_ft(text)` and `meeting_ft(title, summaryText)` via `t.synchronize(withTable:)`,
   `t.tokenizer = .unicode61()`. Acceptance `[ci]`: migrator runs on an empty in-memory queue;
   `PRAGMA foreign_key_check` empty; an inserted segment is found via
   `FTS5Pattern(matchingAllTokensIn:)`; `appliedIdentifiers == ["v1"]`; `SchemaSnapshotTests` dumps
   `SELECT sql FROM sqlite_master ORDER BY name` and compares with `snapshots/schema/v1.sql` (a
   later migration adds `v2.sql` and keeps the `v1`-only dump byte-identical).
4. **MeetingStore CRUD, export, search, observation, merges, confirm (1 d).** Acceptance `[ci]`:
   unit tests per method; `observeMeetings()` and `observeDeliveries` yield a second value after a
   `save`; `search("Jérôme")` matches "jerome" through unicode61; `mergePersons` re-points speakers
   and participants, stores the sample-count-weighted renormalised mean and deletes the loser;
   `mergeSpeakers` moves segments, averages embeddings, deletes the source `Speaker`; `confirm`
   sets `.confirmed`, calls `InMemorySpeakerMemory.enroll` once and removes the clip file; handover
   rows round-trip a device and a receipt.
5. **Settings, secrets, paths, events, intake, clock (0.5 d).** Acceptance `[ci]`: default
   `Settings` has `defaultTemplateID == "default"`, `defaultRetention == .keepDays(30)`,
   `speakerMatchThreshold == 0.60`, `llmContextTokens == 32_000`, `obsidian == nil`; save then load
   round-trips; `FileSecretStore` writes mode 0600; `MeetingEventBus` fans one post out to two
   subscribers; `RecordingIntake` with a fake pipeline calls `enqueue` once and returns the same id
   when called twice with one `recordingID`; `ManualClock.advance(by:)` wakes a sleeper exactly once.
6. **Templates and resources (0.5 d).** Four JSON templates with section ids and headings
   modelled on Jamie's (`2026-09-25-llm-and-templates.md` lists the sections and later edits the
   instruction strings). Acceptance `[ci]`: `SummaryTemplate.bundled.map(\.id)` equals the four
   ids in order; every section has non-empty `id`, `heading`, `instructions`; `Bundle.module`
   loads under `swift test` and from the built binary (spike S2).
7. **Testing support, WAV, fixtures (0.5 d).** Fakes deterministic and configurable (segments per
   lane, cluster count with sample clip ranges, canned `SummaryOutput`), `Fixtures`, `Snapshot`,
   seeded generator (SplitMix64, integer phase accumulators, Int16). Acceptance `[ci]`: decoding
   `audio/sweep-3s.wav` yields 48 000 samples; a 48 kHz or stereo file throws
   `WAVDecodeError.unsupportedFormat`; `Snapshot` fails with a unified diff and rewrites under
   `STENO_UPDATE_SNAPSHOTS=1`; `FixtureManifestTests` regenerates every generated fixture into a
   temp directory and matches `MANIFEST.sha256`.
8. **Stages decode through merge (1 d).** Per lane `decode(asset, lane:)` then transcribe, buffer
   released before the next lane; second-lane hint from the first lane; `meeting.language` =
   duration-weighted `RawSegment.language`; diarize the right lane by `MeetingSource`, decoded
   again; write each cluster's clip through `WAVWriter` to `<meeting folder>/speakers/<id>.wav`;
   `match(_, threshold: settings.speakerMatchThreshold)` → `.suggested` or `.unknown`; copy
   `sampleClipRange` and `clusterConfidence`; lane merge with a deterministic "me" participant for
   `.mic` only. Acceptance `[ci]`: unit tests per stage function with fakes, including language
   election over tagged and untagged segments and one clip file per cluster; `LaneMerger` property
   test: output sorted by `start`, no segment lost, no `.mixed` segment carries the "me" id.
9. **Stages cleanup through retention, failure, reruns, sweep, renderer (1 d).** Any error becomes
   `PipelineFailure` and the one catch in `process` sets `.failed(reason)` with the transcript kept;
   persist calls `decoder.mixdown` for every non-`.m4aAAC` asset; `rerunSummary` runs summarize →
   deliver; `redeliver` runs deliver; `progress` posted per stage; `SummaryMarkdown` renders
   headings at level 2, `- **lead**: text`, labels replaced by current names in bold. Acceptance
   `[ci]`: `PipelineIntegrationTests` drives `process(assetID:)` on an in-memory store with all
   fakes and asserts the final `MeetingExport`, one `RecordingDispatcher` call, `mixdownURL` set
   for `.wav16kInt16`, states `queued → processing → ready` via `observeMeeting`, ten `progress`
   events in `PipelineStage.allCases` order and one `speakersNeedReview`; a throwing
   `FakeSummarizer` yields `.failed` with a `summarize` reason plus intact segments; retention `0`
   sets `expiresAt`; `RetentionSweepTests`: temp folder with master, two sidecars, mixdown, a
   sample clip and an unrelated file → exactly the four removed, row updated, `.keepForever`
   untouched, a missing file does not abort; `SummaryMarkdownTests` golden with a renamed speaker.
10. **CLI and end-to-end target (1 d).** Subcommands as listed, `Wiring.swift`, exit codes (0 ok,
    1 usage, 2 runtime). `EndToEndTests.testMacCallFixtureLandsInVault` with fakes everywhere:
    `audio/conversation-two-lane-6s.wav` through the real `ProcessingPipeline` and `MeetingStore`
    into `RecordingDestination`; asserts meeting `.ready`, `meeting.json` decodes as
    `MeetingExport`, one `Delivery` `.delivered` with receipt, `Meeting.llmUsage` equals the fakes'
    summed usage, event order. Acceptance `[ci]`: `Tests/stenoTests` runs the binary with `HOME`
    set to a temp directory and asserts the real home gains no `Steno/` folder: `dev db migrate`
    on a temp path creates the file; `dev fixtures generate` then `process sweep-3s.wav --source
    mac-in-person` prints a meeting id; `export <id>` writes a `meeting.json` that decodes as
    `MeetingExport`; the end-to-end test passes.

## Tests

Unit (`Tests/StenoCoreTests`, all `[ci]`): `ModelCodableTests`, `MigrationsTests`,
`SchemaSnapshotTests`, `MeetingStoreTests`, `SearchTests`, `SettingsStoreTests`,
`SecretStoreTests`, `EventBusTests`, `RecordingIntakeTests`, `SummaryTemplateTests`,
`WAVAudioDecoderTests`, `SnapshotTests`, `FixtureManifestTests`, `ManualClockTests`,
`LaneMergerTests`, `SummaryMarkdownTests`, `CalendarMatchTests`, `RetentionSweepTests`, one
`*StageTests` per stage function. Integration `[ci]`: `PipelineIntegrationTests` (in-memory store,
all fakes, `macCall` two-lane and `macInPerson` one-lane paths, rerun and failure paths);
`Tests/stenoTests/CLITests` (built binary, temp database, temp `HOME`, fixture WAVs);
`Tests/StenoEndToEndTests` (fakes only at this point). Every test writes into its own temp
directory. No network anywhere; no model downloads.

Manual check `[manual]` (a human on a Mac): `swift run steno dev db migrate`, `steno process` a
fixture with `--audio-folder ~/Desktop/steno-test`, then in `sqlite3` confirm `SELECT * FROM
transcriptSegment_ft WHERE transcriptSegment_ft MATCH 'sweep'` returns rows and that
`steno.sqlite-wal` exists while the CLI runs.

Reviewer trap: any diff above the last `registerMigration` in `Migrations.swift`, a changed
`snapshots/schema/*.sql`, or a changed `exports/` golden without a model change is a rejection.

## Spikes

- S2, before step 6: `Bundle.module` resolves for `steno` launched from `.build/debug` and from a
  copied binary. Fallback: templates as generated Swift string literals, noted in this plan.
- Former S1 (FTS5 on the runner) is settled by the verified GRDB manifest; former S3 (`inout`
  context across `await`) disappeared with the step protocol.

## Needs from other workstreams

- StenoAudio: `AudioDecoder` (CAF, m4a, 48 kHz, mixdown), `EchoCanceller`, `AudioAsset` rows with
  `sidecars16k` via `MeetingStore.save(_:)`.
- StenoSpeech: two `SpeechEngine`s, `Diarizer` filling `sampleClipRange`, `SpeakerMemory` over
  `MeetingStore`; the `--engine <id>` flag in `Wiring.swift`.
- StenoLLM: `LanguageModel`, `TranscriptCleaner`, `MeetingSummarizer` returning `SummaryDocument`;
  template `instructions` and `context` strings.
- StenoAdapters: `DeliveryDispatcher`, `Destination` (Obsidian folder) rendering its own artefacts
  and calling `SummaryMarkdown.render`.
- StenoHandover: calls `MeetingStore` handover rows and `HandoverIntake.admit`.
- macOS app: `SecretStore` on Keychain; consumes `observeMeetings()`, `observeMeeting(id:)`,
  `observeDeliveries`, `MeetingEventBus.subscribe()`, `SettingsStore.observe()`; runs
  `RetentionSweep` at launch and after each meeting; calls `confirm`, `mergePersons`,
  `mergeSpeakers`, `enqueue`, `rerunSummary`, `redeliver`.

## Verified API facts

Every GRDB, swift-argument-parser and Foundation name above was checked against GRDB 7.11.1 sources
and DocC, swift-argument-parser 1.8.2 sources, Apple's `Locale.Language` page, or the correctness
review's verified claims (`Database.BusyMode.timeout(_:)`, `.order(Column.rank)`, FTS5 in GRDB's
SPM build, `swift format lint --strict --recursive`). Still open: `Bundle.module` for a copied
executable (spike S2).

## Deferred

- `SpeakerMemory.forget(personID:)` and person deletion: not in v1 scope; merging split speakers
  is covered by `mergePersons` and `mergeSpeakers`.
- Core's own sample-clip range picker: the diarizer chooses the range; core writes the file.
