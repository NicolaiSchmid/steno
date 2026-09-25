# Steno v1 plans: architecture and elegance review

Date: 2026-09-25. Reviewer: architecture and elegance pass over
`2026-09-25-v1-program.md` and the seven workstream plans on branch
`plans/v1`. Correctness and test coverage are reviewed elsewhere and are not
repeated here. Nothing below edits a plan; each finding proposes a change for
the program document to adopt first.

**major**: expensive once code compiles against it. **minor**: one PR.

## Findings

1. **major** — `v1-program.md` Canonical model (`Settings.destinations`,
   `enabledDestinationIDs`, `DestinationSettings`) and Protocols
   (`Destination`); `adapters-obsidian.md` Public API (`DestinationRegistry`,
   `ObsidianSettings.init(settings:)`, `asDestinationSettings()`).
   A plugin registry for one plugin: settings go `Settings` -> `JSONValue`
   bag keyed by string -> throwing `ObsidianSettings` decode on every
   delivery, plus an actor wrapping an immutable dictionary. The settings UI
   edits an untyped bag, and every future field is a string key in three
   places. Replace with typed settings owned by the destination instance:
   `Settings.obsidian: ObsidianSettings?` (a second destination adds a second
   optional field), `ObsidianFolderDestination(settings: ObsidianSettings)`,
   `protocol Destination: Sendable { var id: String { get }; func validate() async throws;
   func deliver(_ export: MeetingExport, previous: DeliveryReceipt?) async throws -> DeliveryReceipt }`,
   `DeliveryCoordinator(store:, destinations: [any Destination])`. Drop
   `DestinationRegistry`, `DestinationSettings`, `Destination.displayName`
   (the app hard-codes the tab name anyway) and `DeliveryMode`: the adapters
   plan already says a row without receipt "falls back to `.initial`", so
   `.initial == .reexport(previous: nil)` and the enum is one optional.

2. **major** — `v1-program.md` supporting types (`MeetingExport.artifacts`,
   `RenderedArtifact`); `adapters-obsidian.md` Delivery runtime.
   The coordinator renders every artefact under `RenderOptions.plain` into
   the canonical export, then the only destination discards them and
   re-renders with `.wikilink`. Rendering is done twice and the model type
   carries a field only adapters fill. Remove `artifacts` from `MeetingExport`
   and move `RenderedArtifact` (with its `Kind`) into StenoAdapters; each
   destination calls `ArtifactRenderer.render(export, options:)` with its own
   options. Core stops knowing about folder notes and person pages.

3. **major** — `core-foundation.md` Public API (`PipelineStep`,
   `PipelineSteps`, `PipelineContext`, `stage: String`).
   Ten `any PipelineStep` values mutating one `inout` context of optionals
   is a mini framework whose ordering constraints live in nil checks
   (`diarization?`, `summary?`) and whose second injection axis ("tests swap
   any step") has no user: `PipelineDependencies` already swaps every real
   collaborator. Replace with private typed functions on the actor, each
   testable through its inputs: `func transcribe(_ lanes: [AudioLane: AudioBuffer16k]) async throws -> [AudioLane: [RawSegment]]`,
   `func matchSpeakers(_ result: DiarizationResult) async throws -> [Speaker]`, etc.
   Progress uses `enum PipelineStage: String, CaseIterable, Sendable` in
   `MeetingEvent.progress(meetingID:stage:fraction:)` instead of a string;
   the spike S3 about `inout` across `await` disappears with it.

4. **major** — `v1-program.md` model (`Speaker.personID?`, `confidence`);
   `macos-app-and-release.md` Speaker review (`finish()` enrols and re-exports).
   `personID == nil` conflates three states: unknown, auto-matched but not
   yet confirmed, confirmed by the user. After a relaunch the app cannot
   tell which auto-matches still need review, and the rule "enrol on
   confirm" lives in a view model where the CLI cannot reach it.
   `confidence` is the diarizer's cluster quality, not the match score, and
   the name will be misread. Model the state:
   `enum SpeakerAssignment: Codable, Sendable { case unknown; case suggested(personID: UUID, similarity: Float); case confirmed(personID: UUID) }`
   on `Speaker`, rename `confidence` to `clusterConfidence`, and add one core
   operation `MeetingStore.confirm(speakerID:, person: Person, memory: any SpeakerMemory)`
   that sets `.confirmed`, enrols and posts the event. `speakersNeedReview`
   becomes derivable from the rows.

5. **major** — `llm-and-templates.md` decisions 4 and 7, `SummaryRenderer`;
   `v1-program.md` `Meeting.summary` (markdown); `macos-app-and-release.md`
   step 5 versus `llm-and-templates.md` Needs ("re-run summary after speaker
   renaming"). The LLM already returns structured sections (`lead`, `text`)
   and speaker labels verbatim, but StenoLLM renders to Markdown with names
   bolded at generation time and core stores the string. Renaming a speaker
   then needs either a paid re-run (LLM plan) or leaves "Speaker 2" in the
   summary (app plan re-exports only); the two plans disagree. Store the
   structure: `Meeting.summary: SummaryDocument` with
   `struct SummarySection: Codable, Sendable { id: String; heading: String; bullets: [SummaryBullet] }`,
   render Markdown in core at display and export time with the current
   speaker names. Renaming becomes a re-export, `SummaryRenderer` leaves
   StenoLLM, and the app and Obsidian show the same bytes.

6. **major** — `macos-app-and-release.md` Public API of the app target
   (`CaptureControlling`, `MeetingDetecting`, `PipelineControlling`,
   `HandoverControlling`, `engines: SpeechEngineFactory.Type`).
   These protocols wrap module types that are already injectable and
   fake-able (`CaptureSession` over `CaptureBackend`, `ProcessingPipeline`
   over `PipelineDependencies`, `HandoverService` over `HandoverStore` and
   `HandoverIntake`) and rename their members on the way (`forget(id)` for
   `revoke(_:)`, `pairingPayload()` for `beginPairing()`). Passing a
   metatype as a dependency is a factory-of-a-factory. Depend on the module
   types directly and keep app protocols only where a system framework has
   no seam: `CalendarProviding`, `LoginItemControlling`,
   `PermissionsChecking`, `UpdaterControlling`. Replace `engines:` with
   `makeSpeechEngine: @Sendable (SpeechEngineID) throws -> any SpeechEngine`.
   The "overlapping now, else next within 15 minutes" calendar rule is a pure
   function and belongs in StenoCore, not `CalendarService`.

7. **major** — `phone-handover.md` decision 5 and Files
   (`HTTPConnection.swift`, `HTTPRequest.swift`, `HTTPResponse.swift`,
   `Router.swift`). A hand-written HTTP/1.1 parser that the plan itself
   calls "a LAN attack surface" with "strict limits, edge-case tests".
   `swift-nio-transport-services` runs on Network.framework, accepts the
   same `sec_protocol_options` identity, and `NIOHTTP1` parses requests,
   `Expect: 100-continue`, chunked bodies and limits. The Bonjour advertise
   and pinned TLS stay as designed; three parser files, their tests and the
   431/limits logic go. Two packages against a bespoke parser is the right
   trade for the one place Steno accepts bytes from another device.

8. **major** — `core-foundation.md` CLI (`steno export` writes `meeting.json`
   decoding as `MeetingExport`) versus `adapters-obsidian.md` JSON
   (`schema_version`, snake_case, no embeddings, `delivery` block).
   Two files named `meeting.json` with two schemas and two key casings,
   while the handover wire uses camelCase. Agents consuming the vault get a
   different shape from the CLI. One definition: `MeetingExport`'s own
   `Codable` encoding (camelCase, sorted keys, ISO 8601) is `meeting.json`
   everywhere; strip embeddings by giving `Speaker`'s export encoding no
   `embedding` key rather than a second renderer; `schemaVersion` lives on
   `MeetingExport`. `MeetingJSONRenderer` reduces to `JSONEncoder`.

9. **minor** — `core-foundation.md` (`ProcessingPipeline.redeliver`,
   `RecordingIntake.admit`) and `adapters-obsidian.md`
   (`DeliveryCoordinator.reexport(meetingID:destinationID:)`); the app plan
   lists both. Re-export has two entry points with different shapes, and
   "finished recording becomes a queued meeting" is written once for the
   phone (`RecordingIntake`) and again in the app for Mac recordings. Keep
   `ProcessingPipeline.redeliver(meetingID:)` as the only re-export (it owns
   events), delete `reexport(meetingID:destinationID:)` (per-destination
   re-export has no UI in scope), and add
   `ProcessingPipeline.enqueue(_ meeting: Meeting, asset: AudioAsset) async throws`
   that both the app and `RecordingIntake` call.

10. **minor** — `v1-program.md` Protocols (`PersonStore`, `HandoverStore`).
    Each has exactly one implementation, `MeetingStore`, and no fake: the
    speech and handover tests both use `MeetingStore.inMemory()`. AGENTS.md
    asks for two implementations before generalising. Pass `MeetingStore`
    and delete both protocols and `Stores.swift`. `HandoverIntake` and
    `DeliveryDispatcher` stay: they cross the dependency direction.

11. **minor** — `v1-program.md` model, redundant or derived fields.
    `HandoverReceipt.meetingID?` duplicates `.complete(meetingID)`;
    `Delivery.status` carries dates that duplicate `lastAttemptAt` (use
    `.pending`, `.delivered`, `.failed(String)`); `AudioAsset.durationSeconds`
    duplicates `Meeting.duration`; `AudioAsset.expiresAt` is
    `f(retention, readyAt)`; `calendarAttendees: [String]` in
    `CleanupContext`, `SummaryInput` and `PipelineContext` duplicates the
    `Participant` rows the app writes from EventKit (a participant with
    `role == .them` is an attendee). `StenoHandover.HandoverTransfer` is a
    projection of `HandoverReceipt` (`receivedBytes = receivedChunks * chunkSize`);
    let the UI read the receipt.

12. **minor** — `v1-program.md` `SpeakerMemory`. Unlabelled tuples in a
    public protocol, `[Float]` where the model has `Embedding`, and a person
    merge expressed twice with different vocabularies
    (`merge(_ source:into target:)` returning `Person` versus
    `PersonStore.mergePersons(keep:remove:)`). Shape:
    `struct SpeakerMatch: Sendable { person: Person; similarity: Float }`,
    `func candidates(for embedding: Embedding, limit: Int) async throws -> [SpeakerMatch]`,
    `func match(_ embedding: Embedding) async throws -> SpeakerMatch?` as a
    default implementation applying threshold and margin, `enroll(_:as:)`,
    and one merge on `MeetingStore.mergePersons(keep:remove:)` that does the
    three-line weighted mean itself (`sampleCount` is already there).

13. **minor** — `v1-program.md` Templates ("StenoLLM owns prompt wording
    only") versus `TemplateSection.instructions` and `SummaryTemplate.context`,
    which are prompt text written by the LLM workstream into core resources.
    The boundary as drawn puts half the prompt in core JSON and half in
    `SummaryPromptBuilder`; every wording tweak touches two modules and
    core's snapshot tests. Either move `Resources/Templates` and
    `TemplateRegistry` into StenoLLM (core keeps `Meeting.templateID: String`,
    `SummaryInput.templateID`, and the app reads the list from StenoLLM,
    which it links anyway; core loses `Bundle.module` and spike S2), or keep
    them in core and delete the "wording only" sentence. The first is smaller.

14. **minor** — `core-foundation.md` `MeetingEvent`. `stateChanged`,
    `meetingReady` and `deliveryUpdated` are row changes that
    `observeMeetings()` / `observeMeeting(id:)` already deliver through GRDB
    `ValueObservation`; two channels for one fact invites drift. Keep the bus
    for what is not in the database: `progress(meetingID:stage:fraction:)`
    and `speakersNeedReview` (or drop the latter after finding 4).

15. **minor** — `phone-handover.md` `HandoverService` is
    `@MainActor @Observable` inside a non-UI module also driven by
    `steno handover serve`; `CaptureSession` and `MeetingDetector` are actors
    with `AsyncStream`s. One style for leaf-module services: actor plus
    streams (`transfers: AsyncStream<[HandoverReceipt]>`), observation
    wrappers belong in the app's view models.

16. **minor** — naming across the program and plans. `AudioDecoding` is the
    only `-ing` protocol in core; call it `AudioDecoder` like `Diarizer` and
    `EchoCanceller`. `DeliveryDispatcher` / `DeliveryCoordinator` /
    `DestinationRegistry` / `SpeechEngineFactory` / `TemplateRegistry` are
    padding words for a protocol, a struct, a dictionary, a function and an
    array: `MeetingDeliverer`, `[any Destination]`, `makeSpeechEngine`,
    `SummaryTemplate.bundled`. `CleanupContext` / `CleanupResult` beside
    `SummaryInput` / `SummaryOutput` name the same pair two ways. Writes use
    `save`, `upsert` and `insert` on one store; use `save`.

17. **minor** — `phone-handover.md` Public API, phone side. The Swift wire
    types `RecordingMetadata`, the `{state, receivedChunks}` status body,
    `{deviceID, deviceName}` and `{token, macID, macName}` have no TypeScript
    counterparts; `QueuedRecording` uses `id` where Swift uses `recordingID`,
    `sha256: string | null` where Swift has `Data` (base64 by default, while
    the QR uses base64url), and `RecordingMetadata.format: String` where core
    has `AudioFormat`. Add `mobile/modules/steno-link/src/wire.ts` with
    `RecordingMetadata`, `RecordingStatus`, `PairRequest`, `PairResponse`
    mirroring the Swift names and one stated encoding (base64url for every
    digest, on both sides), and type `format` as `AudioFormat`'s raw value.
    Hashing a 30 MB file through `expo-crypto` string digests in JS is the
    wrong side of the module boundary; `steno-link` already owns the file.

18. **minor** — `llm-and-templates.md` `LLMEndpoint: Codable` holds
    `apiKey`; a `Codable` secret ends up in a snapshot or log by accident.
    Drop `Codable` and pass the key to `OpenAICompatibleClient.init` only.
    `audio-capture.md` `CaptureConfiguration.retention` leaks a storage rule
    into capture; the caller sets retention on the `AudioAsset` it saves.

19. **minor** — `llm-and-templates.md` `JSONSchema.validateStrict`,
    `StructuredOutputDecoder` repairs (trailing commas, truncated tail). The
    `Codable` draft types are already the validator: decoding into
    `AnalysisDraft` is the check, and `buildRepair` exists for the failure
    path. Keep the schema builder for the request and the fence stripper;
    delete the strict validator (one unit test over the literal schema
    suffices) and the JSON repair heuristics.

20. **minor** — `core-foundation.md` `Sources/StenoCore/Testing/` (public
    fakes, `Snapshot` writing files) and `Audio/WAVAudioDecoder.swift` ship
    inside the product. A `StenoTestSupport` library target depending on
    StenoCore is the SwiftPM way to share test code; the WAV reader and
    writer move there, and `steno process` uses `AVFoundationAudioCodec`
    once StenoAudio exists (the CLI links every module).

21. **minor** — CLI surface across the plans: thirteen top-level
    subcommands, nine of them developer tools (`capture-spike`, `aec-bench`,
    `bakeoff`, `models`, `llm`, `handover`, `fixtures`, `audio-devices`, `db`).
    Group those under `steno dev` so `record`, `process`, `export`, `deliver`
    read as the product.

## Keep

- `AudioLane` shared by capture and transcript, `.mixed` with its stated
  semantics, `AudioBuffer16k` typed at the rate, and sequential lane
  processing. This is what keeps "me" and "them" deterministic all the way
  to the vault.
- `Meeting.state` as one enum with `.failed(reason)`, failures in cleanup or
  summary keeping the transcript, and `rawText` kept beside `text`. One
  place marks a meeting failed; nothing is lost on an LLM outage.
- `DeliveryReceipt` with owned files, managed blocks and the folder pinned at
  first delivery, plus `Frontmatter.encoded()` instead of interpolation.
  Re-export can never eat a user's file or produce unparsable YAML.
