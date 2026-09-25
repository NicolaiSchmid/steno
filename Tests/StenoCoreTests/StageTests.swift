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
  let cleaner: PassthroughCleaner
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
    cleaner: PassthroughCleaner = PassthroughCleaner(),
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
    await #expect(throws: PipelineFailure.self) {
      _ = try await harness.pipeline.decodeAndTranscribe(asset: asset, meetingID: meeting.id)
    }
    do {
      _ = try await harness.pipeline.decodeAndTranscribe(asset: asset, meetingID: meeting.id)
    } catch let failure as PipelineFailure {
      #expect(failure.stage == .decode)
    }
    struct Boom: Error {}
    let failing = try await PipelineHarness(engine: FakeSpeechEngine(failure: Boom()))
    defer { failing.cleanUp() }
    do {
      _ = try await failing.pipeline.decodeAndTranscribe(
        asset: harness.meeting(source: .macInPerson).1, meetingID: meeting.id)
      Issue.record("expected a transcribe failure")
    } catch let failure as PipelineFailure {
      #expect(failure.stage == .transcribe)
      #expect(failure.reason.contains("Boom"))
    }
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
