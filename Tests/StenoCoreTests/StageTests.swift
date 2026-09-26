import Foundation
import Testing

@testable import StenoCore

@Suite struct DecodeTranscribeStageTests {
  @Test func transcribesEachLaneAndPassesTheFirstLanguageAsHint() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, asset) = try harness.meeting(source: .macCall)
    let transcription = try await harness.pipeline.decodeAndTranscribe(
      asset: asset, meetingID: meeting.id)
    #expect(transcription.lanes[.mic]?.count == 6)
    #expect(transcription.lanes[.system]?.count == 6)
    #expect(transcription.language == LanguageTag(rawValue: "de"))
    let calls = await harness.engine.transcriptions.entries
    #expect(calls.map(\.hint) == [nil, LanguageTag(rawValue: "de").language])
    #expect(calls.map(\.duration) == [6, 6])
    #expect(await harness.engine.preparations.count == 1)
  }

  @Test func decodeFailureCarriesTheDecodeStage() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    var (meeting, asset) = try harness.meeting(source: .macInPerson)
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
        asset: try harness.meeting(source: .macInPerson).1, meetingID: meeting.id)
    }
    #expect(transcribeFailure?.stage == .transcribe)
    #expect(transcribeFailure?.reason.contains("Boom") == true)
  }

  @Test func languageElectionWeighsByDuration() {
    let de = LanguageTag(rawValue: "de")
    let en = LanguageTag(rawValue: "en")
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
    let (meeting, asset) = try harness.meeting(source: .macCall)
    _ = try await harness.pipeline.decodeAndTranscribe(asset: asset, meetingID: meeting.id)
    var iterator = stream.makeAsyncIterator()
    #expect(await iterator.next() == .progress(meetingID: meeting.id, stage: .decode))
    #expect(
      await iterator.next() == .progress(meetingID: meeting.id, stage: .transcribe))
    _ = try await harness.pipeline.matchSpeakers(
      [], meetingID: meeting.id, settings: harness.settings)
    #expect(
      await iterator.next()
        == .progress(meetingID: meeting.id, stage: .matchSpeakers))
  }
}

@Suite struct DiarizeStageTests {
  @Test func diarizesTheSystemLaneOfACallAndWritesOneClipPerCluster() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, asset) = try harness.meeting(source: .macCall)
    let diarization = try await harness.pipeline.diarize(
      asset: asset, meeting: meeting)
    #expect(diarization.clusterSpeakers.map(\.speakerID) == diarization.speakers.map(\.id))
    #expect(
      diarization.clusterSpeakers.map(\.ranges) == [[0...1.5, 3...4.5], [1.5...3, 4.5...6]])
    #expect(diarization.speakers.map(\.clusterLabel) == ["Speaker 1", "Speaker 2"])
    #expect(diarization.speakers.allSatisfy { $0.assignment == .unknown })
    #expect(diarization.speakers.map(\.clusterConfidence) == [0.9, 0.8])
    #expect(diarization.speakers.map(\.sampleClipRange) == [0...1.5, 1.5...3])
    #expect(
      diarization.speakers.map(\.embedding) == [
        SampleData.embedding(axis: 0), SampleData.embedding(axis: 1),
      ])
    #expect(
      diarization.speakers[0].id == UUID(derivedFrom: meeting.id, salt: "speaker-Speaker 1"))
    let folder = RecordingLayout(asset: asset).speakersDirectory
    let clips = try FileManager.default.contentsOfDirectory(atPath: folder.path).sorted()
    #expect(clips == diarization.speakers.map { "\($0.id.uuidString).wav" }.sorted())
    for speaker in diarization.speakers {
      let clip = try WAVAudioDecoder.read(try #require(speaker.sampleClipURL))
      #expect(clip.duration == 1.5)
    }
    #expect(await harness.diarizer.diarizations.entries == [6])
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
    let (meeting, asset) = try harness.meeting(source: .macInPerson)
    let diarization = try await harness.pipeline.diarize(
      asset: asset, meeting: meeting)
    #expect(diarization.speakers.count == 1)
    let clip = try WAVAudioDecoder.read(try #require(diarization.speakers[0].sampleClipURL))
    #expect(clip.duration == 6)
    #expect(ProcessingPipeline.diarizedLane(source: .macInPerson, lanes: [.mixed]) == .mixed)
    #expect(ProcessingPipeline.diarizedLane(source: .macCall, lanes: [.mic, .system]) == .system)
    #expect(ProcessingPipeline.diarizedLane(source: .macCall, lanes: [.mixed]) == .mixed)
    #expect(ProcessingPipeline.diarizedLane(source: .phone, lanes: []) == nil)
  }
}

@Suite struct DiarizeLabelTests {
  @Test func twoClustersWithOneLabelFailTheStage() async throws {
    let doubled = FakeDiarizer(result: { _ in
      DiarizationResult(clusters: [
        SpeakerCluster(label: "Speaker 1", ranges: [0...3], clusterConfidence: 0.5),
        SpeakerCluster(label: "Speaker 1", ranges: [3...6], clusterConfidence: 0.5),
      ])
    })
    let harness = try await PipelineHarness(diarizer: doubled)
    defer { harness.cleanUp() }
    let (meeting, asset) = try harness.meeting(source: .macInPerson)
    let failure = await #expect(throws: PipelineFailure.self) {
      _ = try await harness.pipeline.diarize(asset: asset, meeting: meeting)
    }
    #expect(failure?.stage == .diarize)
    #expect(failure?.reason.contains("Speaker 1") == true)
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
    let (meeting, asset) = try harness.meeting(source: .macCall)
    try await harness.store.save(meeting, asset: asset)
    let transcription = try await harness.pipeline.decodeAndTranscribe(
      asset: asset, meetingID: meeting.id)
    let diarization = try await harness.pipeline.diarize(
      asset: asset, meeting: meeting)
    let merged = try await harness.pipeline.merge(
      meeting: meeting, lanes: transcription.lanes, diarization: diarization)

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
    let (meeting, asset) = try harness.meeting(source: .macCall)
    try await harness.store.save(meeting, asset: asset)
    for person in SampleData.persons() { try await harness.store.save(person) }
    try await harness.store.save(
      Participant(
        id: SampleData.uuid(30), meetingID: meeting.id, personID: SampleData.personNicolaiID,
        displayName: "Nicolai", role: .me))
    let none = ProcessingPipeline.Diarization(speakers: [], clusterSpeakers: [])
    let merged = try await harness.pipeline.merge(
      meeting: meeting, lanes: [.mic: [RawSegment(start: 0, end: 1, text: "hi")]],
      diarization: none)
    #expect(merged.speakers.map(\.assignment) == [.confirmed(personID: SampleData.personNicolaiID)])
    let export = try await harness.store.export(meetingID: meeting.id)
    #expect(export.participants.count == 1)
    #expect(
      export.displayName(forSpeaker: LaneMerger.meSpeakerID(meetingID: meeting.id)) == "Nicolai")

    let room = try await harness.pipeline.merge(
      meeting: meeting, lanes: [.mixed: [RawSegment(start: 0, end: 1, text: "room")]],
      diarization: none)
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
    let (meeting, asset) = try harness.meeting(source: .macCall)
    try await harness.store.save(meeting, asset: asset)
    try await harness.store.replaceTranscript(
      meeting, segments: SampleData.segments(), speakers: SampleData.speakers())
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
    let (meeting, asset) = try harness.meeting(source: .macCall)
    try await harness.store.save(meeting, asset: asset)
    try await harness.store.replaceTranscript(
      meeting, segments: SampleData.segments(), speakers: SampleData.speakers())
    return (harness, meeting)
  }

  @Test func anUnknownTemplateFailsTheStageWithoutCallingTheModel() async throws {
    let (harness, meeting) = try await Self.prepared()
    defer { harness.cleanUp() }
    var unknown = meeting
    unknown.templateID = "nope"
    let failure = await #expect(throws: PipelineFailure.self) {
      _ = try await harness.pipeline.summarize(
        meeting: unknown, segments: SampleData.segments(), speakers: SampleData.speakers())
    }
    #expect(failure == PipelineFailure(stage: .summarize, reason: "unknown summary template nope"))
    #expect(await harness.summarizer.summaries.count == 0)
    #expect(try await harness.store.meeting(id: meeting.id)?.summary == nil)
  }

  @Test func theModelTitleReplacesAPlainOneAndUsageSums() async throws {
    let (harness, meeting) = try await Self.prepared()
    defer { harness.cleanUp() }
    let prior = LLMUsage(promptTokens: 100, completionTokens: 50, requests: 1)
    var withUsage = meeting
    withUsage.llmUsage = prior
    let updated = try await harness.pipeline.summarize(
      meeting: withUsage, segments: SampleData.segments(), speakers: SampleData.speakers())
    #expect(await harness.summarizer.summaries.entries == ["default"])
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

  @Test func theModelsNameGuessesArePersistedWithTheSummary() async throws {
    let canned = SampleData.summaryOutput()
    let (harness, meeting) = try await Self.prepared(summarizer: FakeSummarizer(canned: canned))
    defer { harness.cleanUp() }
    _ = try await harness.pipeline.summarize(
      meeting: meeting, segments: SampleData.segments(), speakers: SampleData.speakers())
    #expect(
      try await harness.store.nameSuggestions(meetingID: meeting.id) == canned.speakerNames)
    #expect(canned.speakerNames.map(\.name) == ["Jérôme"])
  }

  @Test func calendarTitlesAndEmptyModelTitlesLeaveTheTitleAlone() async throws {
    var canned = SampleData.summaryOutput()
    canned.title = "Model title"
    canned.language = LanguageTag(rawValue: "fr")
    let (harness, meeting) = try await Self.prepared(summarizer: FakeSummarizer(canned: canned))
    defer { harness.cleanUp() }

    var interview = meeting
    interview.templateID = "interview"
    var scheduled = interview
    scheduled.calendarEventID = "event-1"
    let kept = try await harness.pipeline.summarize(
      meeting: scheduled, segments: SampleData.segments(), speakers: SampleData.speakers())
    #expect(kept.title == "Untitled", "a calendar title is authoritative")
    #expect(kept.language == LanguageTag(rawValue: "fr"))
    #expect(kept.templateID == "interview")
    #expect(kept.summary?.templateID == "interview")
    #expect(kept.llmUsage == canned.usage, "no prior usage and none on the meeting")

    let replaced = try await harness.pipeline.summarize(
      meeting: interview, segments: SampleData.segments(), speakers: SampleData.speakers())
    #expect(replaced.title == "Model title")

    canned.title = ""
    let untitled = try await PipelineHarness(
      summarizer: FakeSummarizer(canned: canned), sharedStore: harness.store)
    defer { untitled.cleanUp() }
    let unchanged = try await untitled.pipeline.summarize(
      meeting: meeting, segments: SampleData.segments(), speakers: SampleData.speakers())
    #expect(unchanged.title == "Untitled", "an empty model title never replaces the meeting's")
    #expect(try await harness.store.meeting(id: meeting.id)?.title == "Untitled")
  }
}

@Suite struct PersistStageTests {
  @Test func anAACAssetGetsNoMixdownAndAConfirmedCastPostsNoReview() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, template) = try harness.meeting(source: .phone)
    var asset = template
    asset.format = .m4aAAC
    try await harness.store.save(meeting, asset: asset)
    try await harness.store.replaceTranscript(
      meeting, segments: [],
      speakers: [LaneMerger.meSpeaker(meetingID: meeting.id, personID: SampleData.personNicolaiID)])
    let stream = await harness.events.subscribe()

    let persisted = try await harness.pipeline.persist(meeting: meeting, asset: asset)

    #expect(persisted.mixdownURL == nil)
    #expect(try await harness.store.asset(id: asset.id) == persisted)
    #expect(try await harness.store.meeting(id: meeting.id)?.state == .ready)
    #expect(
      !FileManager.default.fileExists(atPath: RecordingLayout(asset: asset).mixdown(.m4aAAC).path))
    let sentinel = MeetingEvent.progress(meetingID: meeting.id, stage: .retention)
    await harness.events.post(sentinel)
    var iterator = stream.makeAsyncIterator()
    #expect(
      await iterator.next() == .progress(meetingID: meeting.id, stage: .persist))
    #expect(await iterator.next() == sentinel, "no speakersNeedReview when everyone is confirmed")
  }

  @Test func aWAVAssetGetsAMixdownAndOnlyUnconfirmedSpeakersNeedReview() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, asset) = try harness.meeting(source: .macInPerson)
    try await harness.store.save(meeting, asset: asset)
    try await harness.store.replaceTranscript(
      meeting, segments: [], speakers: SampleData.speakers())
    let stream = await harness.events.subscribe()

    let persisted = try await harness.pipeline.persist(meeting: meeting, asset: asset)

    let mixdown = RecordingLayout(asset: asset).mixdown(.wav16kInt16)
    #expect(persisted.mixdownURL == mixdown, "named after the decoder's mixdownFormat")
    #expect(mixdown.lastPathComponent == "audio.wav")
    #expect(try Data(contentsOf: mixdown) == Data(contentsOf: asset.url))
    #expect(try await harness.store.asset(id: asset.id)?.mixdownURL == mixdown)
    var iterator = stream.makeAsyncIterator()
    #expect(
      await iterator.next() == .progress(meetingID: meeting.id, stage: .persist))
    #expect(
      await iterator.next()
        == .speakersNeedReview(meetingID: meeting.id, speakerIDs: [SampleData.speakerTwoID]))
  }
}

@Suite struct RetentionStageTests {
  @Test func retentionAppliedFollowsTheExpiryWrite() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, asset) = try harness.meeting(
      source: .phone, retention: .deleteAfterProcessing)
    var ready = meeting
    ready.state = .ready
    try await harness.store.save(ready, asset: asset)
    let stream = await harness.events.subscribe()

    try await harness.pipeline.retention(asset: asset)

    #expect(try await harness.store.asset(id: asset.id)?.expiresAt == PipelineHarness.now)
    #expect(
      await harness.events.drain(stream) == [
        .progress(meetingID: meeting.id, stage: .retention),
        .retentionApplied(meetingID: meeting.id),
      ])
  }

  @Test func aFailedExpiryWritePostsNoRetentionApplied() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    // The meeting row was never written, so the foreign key refuses the
    // asset row and the stage throws before anything could be swept.
    let (meeting, asset) = try harness.meeting(source: .phone)
    let stream = await harness.events.subscribe()

    let failure = await #expect(throws: PipelineFailure.self) {
      try await harness.pipeline.retention(asset: asset)
    }

    #expect(failure?.stage == .retention)
    #expect(try await harness.store.asset(id: asset.id) == nil)
    #expect(
      await harness.events.drain(stream) == [.progress(meetingID: meeting.id, stage: .retention)],
      "a sweep started on the event must find the expiry written")
  }
}
