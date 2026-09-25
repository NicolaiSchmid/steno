import Foundation
import Testing

@testable import StenoCore

/// A pipeline over an in-memory store, temp audio folder and every fake,
/// with hooks to swap single dependencies.
struct PipelineHarness {
  let directory: URL
  let store: MeetingStore
  let settingsStore: SettingsStore
  let events: MeetingEventBus
  let engine: FakeSpeechEngine
  let diarizer: FakeDiarizer
  let memory: InMemorySpeakerMemory
  let cleaner: any TranscriptCleaner
  let summarizer: FakeSummarizer
  let destination: RecordingDestination
  let dispatcher: RecordingDispatcher
  let pipeline: ProcessingPipeline
  var settings: Settings

  static let now = SampleData.updatedAt

  init(
    engine: FakeSpeechEngine = FakeSpeechEngine(),
    diarizer: FakeDiarizer = FakeDiarizer(),
    memory: InMemorySpeakerMemory = InMemorySpeakerMemory(people: SampleData.persons()),
    cleaner: any TranscriptCleaner = PassthroughCleaner(),
    summarizer: FakeSummarizer = FakeSummarizer(),
    retention: AudioRetention = .keepDays(30),
    sharedStore: MeetingStore? = nil
  ) async throws {
    directory = try Fixtures.temporaryDirectory("pipeline")
    store = try sharedStore ?? MeetingStore.inMemory()
    settingsStore = SettingsStore(writer: store.writer)
    settings = Settings()
    settings.audioFolder = directory.appendingPathComponent("audio", isDirectory: true)
    settings.defaultRetention = retention
    try await settingsStore.save(settings)
    // The production SpeakerMemory reads persons from the store, so every
    // person the in-memory fake can suggest must exist as a row.
    for person in SampleData.persons() { try await self.store.save(person) }
    events = MeetingEventBus()
    self.engine = engine
    self.diarizer = diarizer
    self.memory = memory
    self.cleaner = cleaner
    self.summarizer = summarizer
    destination = RecordingDestination(
      root: directory.appendingPathComponent("vault", isDirectory: true))
    dispatcher = RecordingDispatcher(store: store, destinations: [destination], now: { Self.now })
    pipeline = ProcessingPipeline(
      dependencies: PipelineDependencies(
        decoder: WAVAudioDecoder(), speechEngine: engine, diarizer: diarizer, speakerMemory: memory,
        cleaner: cleaner, summarizer: summarizer, delivery: dispatcher, store: store,
        settings: settingsStore, events: events, now: { Self.now }))
  }

  func cleanUp() {
    try? FileManager.default.removeItem(at: directory)
  }

  /// A queued mac call with the two-lane fixture as master and per-lane
  /// sidecars, or a one-lane in-person recording.
  func meeting(source: MeetingSource, retention: AudioRetention = .keepDays(30)) -> (
    Meeting, AudioAsset
  ) {
    let meeting = Meeting(
      id: SampleData.meetingID, title: "Untitled", startedAt: SampleData.startedAt, duration: 6,
      source: source, state: .recording, createdAt: SampleData.createdAt,
      updatedAt: SampleData.createdAt)
    let asset: AudioAsset
    switch source {
    case .macCall:
      asset = AudioAsset(
        id: SampleData.uuid(70), meetingID: meeting.id,
        url: Fixtures.url("audio/conversation-two-lane-6s.wav"), format: .wav16kInt16,
        lanes: [.mic, .system],
        sidecars16k: [
          .mic: Fixtures.url("audio/conversation-mic-6s.wav"),
          .system: Fixtures.url("audio/conversation-system-6s.wav"),
        ],
        retention: retention)
    case .macInPerson, .phone:
      asset = AudioAsset(
        id: SampleData.uuid(70), meetingID: meeting.id,
        url: Fixtures.url("audio/conversation-two-lane-6s.wav"), format: .wav16kInt16,
        lanes: [.mixed], retention: retention)
    }
    return (meeting, asset)
  }
}

@Suite struct DecodeTranscribeStageTests {
  @Test func transcribesEachLaneAndPassesTheFirstLanguageAsHint() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, asset) = harness.meeting(source: .macCall)
    let transcription = try await harness.pipeline.decodeAndTranscribe(
      asset: asset, meetingID: meeting.id)
    #expect(transcription.lanes[.mic]?.count == 6)
    #expect(transcription.lanes[.system]?.count == 6)
    #expect(transcription.language == Locale.Language(stenoIdentifier: "de"))
    let calls = await harness.engine.calls.calls
    #expect(calls.map(\.hint) == [nil, Locale.Language(stenoIdentifier: "de")])
    #expect(calls.map(\.duration) == [6, 6])
    #expect(await harness.engine.prepareCalls.count == 1)
  }

  @Test func decodeFailureCarriesTheDecodeStage() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    var (meeting, asset) = harness.meeting(source: .macInPerson)
    asset.url = harness.directory.appendingPathComponent("missing.wav")
    meeting.state = .queued
    let decodeFailure = await #expect(throws: PipelineFailure.self) {
      _ = try await harness.pipeline.decodeAndTranscribe(asset: asset, meetingID: meeting.id)
    }
    #expect(decodeFailure?.stage == .decode)
    struct Boom: Error {}
    let failing = try await PipelineHarness(engine: FakeSpeechEngine(failure: Boom()))
    defer { failing.cleanUp() }
    let transcribeFailure = await #expect(throws: PipelineFailure.self) {
      _ = try await failing.pipeline.decodeAndTranscribe(
        asset: harness.meeting(source: .macInPerson).1, meetingID: meeting.id)
    }
    #expect(transcribeFailure?.stage == .transcribe)
    #expect(transcribeFailure?.reason.contains("Boom") == true)
  }

  @Test func languageElectionWeighsByDuration() {
    let de = Locale.Language(stenoIdentifier: "de")
    let en = Locale.Language(stenoIdentifier: "en")
    #expect(LanguageElection.elect([]) == nil)
    #expect(LanguageElection.elect([RawSegment(start: 0, end: 5, text: "x")]) == nil)
    let segments = [
      RawSegment(start: 0, end: 1, text: "a", language: de),
      RawSegment(start: 1, end: 1.5, text: "b", language: de),
      RawSegment(start: 2, end: 4, text: "c", language: en),
      RawSegment(start: 4, end: 9, text: "d"),
    ]
    #expect(LanguageElection.elect(segments) == en)
    #expect(LanguageElection.elect(Array(segments.prefix(2))) == de)
    let tie = [
      RawSegment(start: 0, end: 1, text: "a", language: en),
      RawSegment(start: 1, end: 2, text: "b", language: de),
    ]
    #expect(LanguageElection.elect(tie) == de)
  }

  @Test func progressIsPostedOncePerStageAcrossLanes() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let stream = await harness.events.subscribe()
    let (meeting, asset) = harness.meeting(source: .macCall)
    _ = try await harness.pipeline.decodeAndTranscribe(asset: asset, meetingID: meeting.id)
    var iterator = stream.makeAsyncIterator()
    #expect(await iterator.next() == .progress(meetingID: meeting.id, stage: .decode, fraction: 0))
    #expect(
      await iterator.next() == .progress(meetingID: meeting.id, stage: .transcribe, fraction: 0.1))
    _ = try await harness.pipeline.matchSpeakers(
      [], meetingID: meeting.id, settings: harness.settings)
    #expect(
      await iterator.next()
        == .progress(meetingID: meeting.id, stage: .matchSpeakers, fraction: 0.3))
  }
}

@Suite struct DiarizeStageTests {
  @Test func diarizesTheSystemLaneOfACallAndWritesOneClipPerCluster() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, asset) = harness.meeting(source: .macCall)
    let diarization = try await harness.pipeline.diarize(
      asset: asset, meeting: meeting, settings: harness.settings)
    #expect(diarization.clusters.count == 2)
    #expect(diarization.speakers.map(\.clusterLabel) == ["Speaker 1", "Speaker 2"])
    #expect(diarization.speakers.allSatisfy { $0.assignment == .unknown })
    #expect(diarization.speakers.map(\.clusterConfidence) == [0.9, 0.8])
    #expect(diarization.speakers.map(\.sampleClipRange) == [0...1.5, 1.5...3])
    #expect(
      diarization.speakers.map(\.embedding) == [
        SampleData.embedding(axis: 0), SampleData.embedding(axis: 1),
      ])
    #expect(
      diarization.speakers[0].id == MeetingStore.derivedID(meeting.id, salt: "speaker-Speaker 1"))
    let folder = ProcessingPipeline.meetingFolder(meeting.id, settings: harness.settings)
      .appendingPathComponent("speakers", isDirectory: true)
    let clips = try FileManager.default.contentsOfDirectory(atPath: folder.path).sorted()
    #expect(clips == diarization.speakers.map { "\($0.id.uuidString).wav" }.sorted())
    for speaker in diarization.speakers {
      let clip = try WAVAudioDecoder.read(try #require(speaker.sampleClipURL))
      #expect(clip.duration == 1.5)
    }
    #expect(await harness.diarizer.calls.calls == [6])
  }

  @Test func inPersonDiarizesTheMixedLaneAndCapsClipsAtTenSeconds() async throws {
    let long = FakeDiarizer(result: { _ in
      DiarizationResult(clusters: [
        SpeakerCluster(
          label: "Speaker 1", ranges: [0...6], clusterConfidence: 0.5, sampleClipRange: 0...30)
      ])
    })
    let harness = try await PipelineHarness(diarizer: long)
    defer { harness.cleanUp() }
    let (meeting, asset) = harness.meeting(source: .macInPerson)
    let diarization = try await harness.pipeline.diarize(
      asset: asset, meeting: meeting, settings: harness.settings)
    #expect(diarization.speakers.count == 1)
    let clip = try WAVAudioDecoder.read(try #require(diarization.speakers[0].sampleClipURL))
    #expect(clip.duration == 6)
    #expect(ProcessingPipeline.diarizedLane(source: .macInPerson, lanes: [.mixed]) == .mixed)
    #expect(ProcessingPipeline.diarizedLane(source: .macCall, lanes: [.mic, .system]) == .system)
    #expect(ProcessingPipeline.diarizedLane(source: .macCall, lanes: [.mixed]) == .mixed)
    #expect(ProcessingPipeline.diarizedLane(source: .phone, lanes: []) == nil)
  }
}

@Suite struct MatchSpeakersStageTests {
  @Test func matchesAboveThresholdAndLeavesTheRestUnknown() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    var tilted = [Float](repeating: 0, count: Embedding.dimension)
    tilted[0] = 0.8
    tilted[1] = 0.6
    let speakers = [
      Speaker(
        id: SampleData.uuid(20), meetingID: SampleData.meetingID, clusterLabel: "Speaker 1",
        embedding: SampleData.embedding(axis: 0), clusterConfidence: 0.9),
      Speaker(
        id: SampleData.uuid(21), meetingID: SampleData.meetingID, clusterLabel: "Speaker 2",
        embedding: SampleData.embedding(axis: 5), clusterConfidence: 0.8),
      Speaker(
        id: SampleData.uuid(22), meetingID: SampleData.meetingID, clusterLabel: "Speaker 3",
        embedding: nil, clusterConfidence: 0.7),
      Speaker(
        id: SampleData.uuid(23), meetingID: SampleData.meetingID, clusterLabel: "Speaker 4",
        embedding: Embedding(tilted), clusterConfidence: 0.7),
    ]
    let matched = try await harness.pipeline.matchSpeakers(
      speakers, meetingID: SampleData.meetingID, settings: harness.settings)
    #expect(
      matched[0].assignment == .suggested(personID: SampleData.personNicolaiID, similarity: 1))
    #expect(matched[1].assignment == .unknown)
    #expect(matched[2].assignment == .unknown)
    // 0.8 vs 0.6: above the 0.60 threshold and more than 0.05 apart.
    #expect(matched[3].assignment.personID == SampleData.personNicolaiID)

    var strict = harness.settings
    strict.speakerMatchThreshold = 0.95
    let strictMatch = try await harness.pipeline.matchSpeakers(
      speakers, meetingID: SampleData.meetingID, settings: strict)
    #expect(strictMatch[3].assignment == .unknown)
    #expect(strictMatch[0].assignment.personID == SampleData.personNicolaiID)
  }
}

@Suite struct MergeStageTests {
  @Test func mergePersistsTranscriptAndCreatesMeForCalls() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, asset) = harness.meeting(source: .macCall)
    try await harness.store.save(meeting, asset: asset)
    let transcription = try await harness.pipeline.decodeAndTranscribe(
      asset: asset, meetingID: meeting.id)
    let diarization = try await harness.pipeline.diarize(
      asset: asset, meeting: meeting, settings: harness.settings)
    let merged = try await harness.pipeline.merge(
      meeting: meeting, lanes: transcription.lanes, clusters: diarization.clusters,
      speakers: diarization.speakers)

    #expect(merged.segments.count == 12)
    #expect(merged.segments == merged.segments.sorted { $0.start < $1.start })
    #expect(merged.speakers.map(\.clusterLabel) == ["Speaker 1", "Speaker 2", "Me"])
    let meID = LaneMerger.meSpeakerID(meetingID: meeting.id)
    #expect(merged.segments.filter { $0.lane == .mic }.allSatisfy { $0.speakerID == meID })
    #expect(
      merged.segments.filter { $0.lane == .system }.allSatisfy {
        $0.speakerID != meID && $0.speakerID != nil
      })

    let export = try await harness.store.export(meetingID: meeting.id)
    #expect(export.segments == merged.segments)
    #expect(export.speakers.count == 3)
    #expect(export.participants.map(\.role) == [.me])
    #expect(export.participants.first?.displayName == "Me")
    #expect(export.participants.first?.id == LaneMerger.meParticipantID(meetingID: meeting.id))
    #expect(export.displayName(forSpeaker: meID) == "Me")
  }

  @Test func mergeUsesAnExistingMeParticipantAndSkipsMeForRoomLanes() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, asset) = harness.meeting(source: .macCall)
    try await harness.store.save(meeting, asset: asset)
    for person in SampleData.persons() { try await harness.store.save(person) }
    try await harness.store.save(
      Participant(
        id: SampleData.uuid(30), meetingID: meeting.id, personID: SampleData.personNicolaiID,
        displayName: "Nicolai", role: .me))
    let merged = try await harness.pipeline.merge(
      meeting: meeting, lanes: [.mic: [RawSegment(start: 0, end: 1, text: "hi")]], clusters: [],
      speakers: [])
    #expect(merged.speakers.map(\.assignment) == [.confirmed(personID: SampleData.personNicolaiID)])
    let export = try await harness.store.export(meetingID: meeting.id)
    #expect(export.participants.count == 1)
    #expect(
      export.displayName(forSpeaker: LaneMerger.meSpeakerID(meetingID: meeting.id)) == "Nicolai")

    let room = try await harness.pipeline.merge(
      meeting: meeting, lanes: [.mixed: [RawSegment(start: 0, end: 1, text: "room")]], clusters: [],
      speakers: [])
    #expect(room.speakers.isEmpty)
    #expect(room.segments.first?.speakerID == nil)
  }
}

@Suite struct CleanupStageTests {
  /// A cleaner that applies `transform` to the segments it is given.
  struct ScriptedCleaner: TranscriptCleaner, Sendable {
    var transform: @Sendable ([TranscriptSegment]) -> [TranscriptSegment]
    let usage = LLMUsage(promptTokens: 7, completionTokens: 3, requests: 1)

    func clean(_ input: CleanupInput) async throws -> CleanupOutput {
      CleanupOutput(segments: transform(input.segments), failedChunks: [], usage: usage)
    }
  }

  /// A harness whose store already holds the sample transcript.
  static func prepared(cleaner: any TranscriptCleaner) async throws -> (PipelineHarness, Meeting) {
    let harness = try await PipelineHarness(cleaner: cleaner)
    let (meeting, asset) = harness.meeting(source: .macCall)
    try await harness.store.save(meeting, asset: asset)
    try await harness.store.replaceTranscript(
      meetingID: meeting.id, segments: SampleData.segments(), speakers: SampleData.speakers())
    return (harness, meeting)
  }

  @Test func onlyTheCleanedTextIsTakenAndItIsPersisted() async throws {
    let cleaner = ScriptedCleaner { segments in
      segments.map { segment in
        var tampered = segment
        tampered.text = segment.text.uppercased()
        tampered.rawText = "tampered"
        tampered.speakerID = nil
        tampered.start += 100
        tampered.lane = .mixed
        return tampered
      }
    }
    let (harness, meeting) = try await Self.prepared(cleaner: cleaner)
    defer { harness.cleanUp() }
    let cleaned = try await harness.pipeline.cleanup(
      meeting: meeting, segments: SampleData.segments(), speakers: SampleData.speakers())
    var expected = SampleData.segments()
    for index in expected.indices { expected[index].text = expected[index].text.uppercased() }
    #expect(cleaned.segments == expected, "ids, order, rawText, speaker and lane are the merge's")
    #expect(cleaned.usage == cleaner.usage)
    let export = try await harness.store.export(meetingID: meeting.id)
    #expect(export.segments == expected)
    #expect(export.speakers == SampleData.speakers())
    #expect(try await harness.store.search("neunzig").count == 1)
  }

  @Test func droppingReorderingOrThrowingFailsTheStageAndKeepsTheTranscript() async throws {
    do {
      let (harness, meeting) = try await Self.prepared(
        cleaner: ScriptedCleaner { Array($0.dropLast()) })
      defer { harness.cleanUp() }
      let failure = await #expect(throws: PipelineFailure.self) {
        _ = try await harness.pipeline.cleanup(
          meeting: meeting, segments: SampleData.segments(), speakers: SampleData.speakers())
      }
      #expect(
        failure == PipelineFailure(stage: .cleanup, reason: "cleaner returned 2 segments for 3"))
      #expect(
        try await harness.store.export(meetingID: meeting.id).segments == SampleData.segments())
    }
    do {
      let (harness, meeting) = try await Self.prepared(cleaner: ScriptedCleaner { $0.reversed() })
      defer { harness.cleanUp() }
      let failure = await #expect(throws: PipelineFailure.self) {
        _ = try await harness.pipeline.cleanup(
          meeting: meeting, segments: SampleData.segments(), speakers: SampleData.speakers())
      }
      #expect(failure?.stage == .cleanup)
      #expect(failure?.reason.hasPrefix("cleaner reordered segment") == true)
      #expect(
        try await harness.store.export(meetingID: meeting.id).segments == SampleData.segments())
    }
    do {
      struct Boom: Error {}
      let (harness, meeting) = try await Self.prepared(
        cleaner: PassthroughCleaner(failure: Boom()))
      defer { harness.cleanUp() }
      let failure = await #expect(throws: PipelineFailure.self) {
        _ = try await harness.pipeline.cleanup(
          meeting: meeting, segments: SampleData.segments(), speakers: SampleData.speakers())
      }
      #expect(failure == PipelineFailure(stage: .cleanup, reason: "Boom()"))
      #expect(
        try await harness.store.export(meetingID: meeting.id).segments == SampleData.segments())
    }
  }
}

@Suite struct SummarizeStageTests {
  static func prepared(summarizer: FakeSummarizer = FakeSummarizer()) async throws -> (
    PipelineHarness, Meeting
  ) {
    let harness = try await PipelineHarness(summarizer: summarizer)
    let (meeting, asset) = harness.meeting(source: .macCall)
    try await harness.store.save(meeting, asset: asset)
    try await harness.store.replaceTranscript(
      meetingID: meeting.id, segments: SampleData.segments(), speakers: SampleData.speakers())
    return (harness, meeting)
  }

  @Test func unknownTemplateFallsBackAndTheModelTitleReplacesAPlainOne() async throws {
    let (harness, meeting) = try await Self.prepared()
    defer { harness.cleanUp() }
    let prior = LLMUsage(promptTokens: 100, completionTokens: 50, requests: 1)
    let updated = try await harness.pipeline.summarize(
      meeting: meeting, segments: SampleData.segments(), speakers: SampleData.speakers(),
      templateID: "nope", priorUsage: prior)
    #expect(await harness.summarizer.calls.calls == ["default"])
    #expect(updated.templateID == "default")
    #expect(updated.summary?.templateID == "default")
    #expect(updated.summary?.sections.map(\.id) == SummaryTemplate.bundled[0].sections.map(\.id))
    #expect(updated.title == "Summary of Untitled")
    #expect(updated.llmUsage == prior + harness.summarizer.usage)
    #expect(updated.updatedAt == PipelineHarness.now)

    let export = try await harness.store.export(meetingID: meeting.id)
    #expect(export.meeting == updated)
    #expect(export.tasks.map(\.text) == ["Follow up on Default."])
    #expect(export.decisions.map(\.text) == ["Decision from Speaker 1."])
    #expect(export.segments == SampleData.segments())
  }

  @Test func calendarTitlesAndEmptyModelTitlesLeaveTheTitleAlone() async throws {
    var canned = SampleData.summaryOutput()
    canned.title = "Model title"
    canned.language = Locale.Language(stenoIdentifier: "fr")
    let (harness, meeting) = try await Self.prepared(summarizer: FakeSummarizer(canned: canned))
    defer { harness.cleanUp() }

    var scheduled = meeting
    scheduled.calendarEventID = "event-1"
    let kept = try await harness.pipeline.summarize(
      meeting: scheduled, segments: SampleData.segments(), speakers: SampleData.speakers(),
      templateID: "interview", priorUsage: nil)
    #expect(kept.title == "Untitled", "a calendar title is authoritative")
    #expect(kept.language == Locale.Language(stenoIdentifier: "fr"))
    #expect(kept.templateID == "interview")
    #expect(kept.summary?.templateID == "interview")
    #expect(kept.llmUsage == canned.usage, "no prior usage and none on the meeting")

    let replaced = try await harness.pipeline.summarize(
      meeting: meeting, segments: SampleData.segments(), speakers: SampleData.speakers(),
      templateID: "interview", priorUsage: nil)
    #expect(replaced.title == "Model title")

    canned.title = ""
    let untitled = try await PipelineHarness(
      summarizer: FakeSummarizer(canned: canned), sharedStore: harness.store)
    defer { untitled.cleanUp() }
    let unchanged = try await untitled.pipeline.summarize(
      meeting: meeting, segments: SampleData.segments(), speakers: SampleData.speakers(),
      templateID: "default", priorUsage: nil)
    #expect(unchanged.title == "Untitled", "an empty model title never replaces the meeting's")
    #expect(try await harness.store.meeting(id: meeting.id)?.title == "Untitled")
  }
}

@Suite struct PersistStageTests {
  @Test func anAACAssetGetsNoMixdownAndAConfirmedCastPostsNoReview() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, template) = harness.meeting(source: .phone)
    var asset = template
    asset.format = .m4aAAC
    try await harness.store.save(meeting, asset: asset)
    try await harness.store.replaceTranscript(
      meetingID: meeting.id, segments: [],
      speakers: [LaneMerger.meSpeaker(meetingID: meeting.id, personID: SampleData.personNicolaiID)])
    let stream = await harness.events.subscribe()

    let persisted = try await harness.pipeline.persist(
      meeting: meeting, asset: asset, settings: harness.settings)

    #expect(persisted.mixdownURL == nil)
    #expect(try await harness.store.asset(id: asset.id) == persisted)
    #expect(try await harness.store.meeting(id: meeting.id)?.state == .ready)
    let folder = ProcessingPipeline.meetingFolder(meeting.id, settings: harness.settings)
    #expect(
      !FileManager.default.fileExists(atPath: folder.appendingPathComponent("audio.m4a").path))
    let sentinel = MeetingEvent.progress(meetingID: meeting.id, stage: .retention, fraction: 1)
    await harness.events.post(sentinel)
    var iterator = stream.makeAsyncIterator()
    #expect(
      await iterator.next() == .progress(meetingID: meeting.id, stage: .persist, fraction: 0.7))
    #expect(await iterator.next() == sentinel, "no speakersNeedReview when everyone is confirmed")
  }

  @Test func aWAVAssetGetsAMixdownAndOnlyUnconfirmedSpeakersNeedReview() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, asset) = harness.meeting(source: .macInPerson)
    try await harness.store.save(meeting, asset: asset)
    try await harness.store.replaceTranscript(
      meetingID: meeting.id, segments: [], speakers: SampleData.speakers())
    let stream = await harness.events.subscribe()

    let persisted = try await harness.pipeline.persist(
      meeting: meeting, asset: asset, settings: harness.settings)

    let mixdown = ProcessingPipeline.meetingFolder(meeting.id, settings: harness.settings)
      .appendingPathComponent("audio.m4a")
    #expect(persisted.mixdownURL == mixdown)
    #expect(try Data(contentsOf: mixdown) == Data(contentsOf: asset.url))
    #expect(try await harness.store.asset(id: asset.id)?.mixdownURL == mixdown)
    var iterator = stream.makeAsyncIterator()
    #expect(
      await iterator.next() == .progress(meetingID: meeting.id, stage: .persist, fraction: 0.7))
    #expect(
      await iterator.next()
        == .speakersNeedReview(meetingID: meeting.id, speakerIDs: [SampleData.speakerTwoID]))
  }
}
