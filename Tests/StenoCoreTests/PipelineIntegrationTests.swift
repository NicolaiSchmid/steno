import Foundation
import Testing

@testable import StenoCore

@Suite struct PipelineIntegrationTests {
  actor StateLog {
    var states: [MeetingState] = []
    func append(_ state: MeetingState?) {
      if let state { states.append(state) }
    }
  }

  @Test func macCallRunsEveryStageAndLandsReady() async throws {
    let seen = StateLog()
    let store = try MeetingStore.inMemory()
    var diarizer = FakeDiarizer()
    diarizer.onDiarize = {
      await seen.append(try? await store.meeting(id: SampleData.meetingID)?.state)
    }
    let observedHarness = try await PipelineHarness(diarizer: diarizer, sharedStore: store)
    defer { observedHarness.cleanUp() }

    let events = await observedHarness.events.subscribe()
    let (meeting, asset) = observedHarness.meeting(source: .macCall)
    try await observedHarness.pipeline.enqueue(meeting, asset: asset)
    await observedHarness.pipeline.waitUntilIdle()

    let export = try await observedHarness.store.export(meetingID: meeting.id)
    #expect(export.meeting.state == .ready)
    #expect(export.meeting.title == "Summary of Untitled")
    #expect(export.meeting.language == Locale.Language(stenoIdentifier: "de"))
    #expect(
      export.meeting.llmUsage == LLMUsage(promptTokens: 300, completionTokens: 150, requests: 2))
    #expect(export.meeting.summary?.templateID == "default")
    #expect(
      export.meeting.summary?.sections.map(\.id) == [
        "executive-summary", "full-summary", "open-questions",
      ])
    #expect(export.segments.count == 12)
    #expect(export.segments.filter { $0.lane == .mic }.count == 6)
    #expect(export.speakers.map(\.clusterLabel) == ["Me", "Speaker 1", "Speaker 2"])
    #expect(
      export.speakers.map(\.assignment.personID) == [
        nil, SampleData.personNicolaiID, SampleData.personJeromeID,
      ])
    #expect(export.speakers.allSatisfy { !$0.assignment.isConfirmed })
    #expect(export.participants.map(\.role) == [.me])
    #expect(export.tasks.count == 1)
    #expect(export.decisions.count == 1)
    let audio = try #require(export.audio)
    #expect(
      audio.mixdownURL
        == ProcessingPipeline.meetingFolder(meeting.id, settings: observedHarness.settings)
        .appendingPathComponent("audio.m4a"))
    #expect(FileManager.default.fileExists(atPath: try #require(audio.mixdownURL).path))
    #expect(audio.expiresAt == PipelineHarness.now.addingTimeInterval(30 * 86_400))
    #expect(
      export.speakers.filter { $0.clusterLabel != "Me" }.allSatisfy { $0.sampleClipURL != nil })

    #expect(await observedHarness.dispatcher.calls.calls == [meeting.id])
    let deliveries = try await observedHarness.store.deliveries(meetingID: meeting.id)
    #expect(deliveries.map(\.status) == [.delivered])
    let written = try Data(contentsOf: observedHarness.destination.exportURL(meetingID: meeting.id))
    #expect(try StenoJSON.decode(MeetingExport.self, from: written).meeting.state == .ready)

    #expect(await seen.states == [.processing])

    // A failed run posts fewer events; bail out instead of waiting forever.
    guard export.meeting.state == .ready else { return }
    var iterator = events.makeAsyncIterator()
    var collected: [MeetingEvent] = []
    for _ in 0..<11 {
      if let event = await iterator.next() { collected.append(event) }
    }
    let stages = collected.compactMap { event -> PipelineStage? in
      if case .progress(_, let stage, _) = event { return stage }
      return nil
    }
    #expect(stages == PipelineStage.allCases)
    let reviews = collected.filter {
      if case .speakersNeedReview = $0 { return true }
      return false
    }
    #expect(
      reviews == [.speakersNeedReview(meetingID: meeting.id, speakerIDs: export.speakers.map(\.id))]
    )
    #expect(collected.firstIndex(of: reviews[0]) == 8)
  }

  @Test func observeMeetingSeesTheStatesInOrder() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, asset) = harness.meeting(source: .macInPerson)
    var iterator = harness.store.observeMeeting(id: meeting.id).makeAsyncIterator()
    #expect(try await iterator.next() == .some(nil))
    try await harness.pipeline.enqueue(meeting, asset: asset)
    await harness.pipeline.waitUntilIdle()

    var states: [MeetingState] = []
    while states.last != .ready, states.last?.isFailed != true,
      let next = try await iterator.next()
    {
      if let state = next?.meeting.state, state != states.last { states.append(state) }
    }
    let order: [MeetingState] = [.queued, .processing, .ready]
    #expect(states.last == .ready)
    #expect(
      states.map { order.firstIndex(of: $0) ?? -1 }
        == states.map { order.firstIndex(of: $0) ?? -1 }.sorted())
    #expect(!states.contains { $0.isFailed })
  }

  @Test func inPersonHasNoMeAndAssignsRoomSegmentsToClusters() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, asset) = harness.meeting(source: .macInPerson)
    try await harness.pipeline.enqueue(meeting, asset: asset)
    await harness.pipeline.waitUntilIdle()
    let export = try await harness.store.export(meetingID: meeting.id)
    #expect(export.meeting.state == .ready)
    #expect(export.speakers.map(\.clusterLabel) == ["Speaker 1", "Speaker 2"])
    #expect(export.participants.isEmpty)
    #expect(export.segments.count == 6)
    #expect(export.segments.allSatisfy { $0.lane == .mixed && $0.speakerID != nil })
    #expect(Set(export.segments.compactMap(\.speakerID)) == Set(export.speakers.map(\.id)))
  }

  @Test func summarizeFailureMarksFailedAndKeepsTheTranscript() async throws {
    struct Boom: Error {}
    let harness = try await PipelineHarness(summarizer: FakeSummarizer(failure: Boom()))
    defer { harness.cleanUp() }
    let (meeting, asset) = harness.meeting(source: .macCall)
    try await harness.store.save(meeting, asset: asset)
    let error = await #expect(throws: PipelineFailure.self) {
      try await harness.pipeline.process(assetID: asset.id)
    }
    #expect(error?.stage == .summarize)
    let stored = try #require(try await harness.store.meeting(id: meeting.id))
    #expect(stored.state == .failed(reason: "summarize: Boom()"))
    #expect(try await harness.store.export(meetingID: meeting.id).segments.count == 12)
    #expect(await harness.dispatcher.calls.count == 0)
    #expect(stored.summary == nil)
  }

  @Test func retentionZeroExpiresImmediately() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, asset) = harness.meeting(source: .macInPerson, retention: .deleteAfterProcessing)
    try await harness.pipeline.enqueue(meeting, asset: asset)
    await harness.pipeline.waitUntilIdle()
    let stored = try #require(try await harness.store.asset(id: asset.id))
    #expect(stored.expiresAt == PipelineHarness.now)
    let forever = try await PipelineHarness()
    defer { forever.cleanUp() }
    let (m2, a2) = forever.meeting(source: .macInPerson, retention: .keepForever)
    try await forever.pipeline.enqueue(m2, asset: a2)
    await forever.pipeline.waitUntilIdle()
    #expect(try await forever.store.asset(id: a2.id)?.expiresAt == nil)
  }

  @Test func rerunSummaryAndRedeliver() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, asset) = harness.meeting(source: .macInPerson)
    try await harness.pipeline.enqueue(meeting, asset: asset)
    await harness.pipeline.waitUntilIdle()
    let events = await harness.events.subscribe()

    try await harness.pipeline.rerunSummary(meetingID: meeting.id, templateID: "daily-standup")
    let export = try await harness.store.export(meetingID: meeting.id)
    #expect(export.meeting.state == .ready)
    #expect(export.meeting.templateID == "daily-standup")
    #expect(export.meeting.summary?.templateID == "daily-standup")
    #expect(export.meeting.summary?.sections.count == 4)
    #expect(
      export.meeting.llmUsage == LLMUsage(promptTokens: 500, completionTokens: 250, requests: 3))
    #expect(await harness.summarizer.calls.calls == ["default", "daily-standup"])
    #expect(await harness.dispatcher.calls.count == 2)

    try await harness.pipeline.redeliver(meetingID: meeting.id)
    #expect(await harness.dispatcher.calls.count == 3)
    #expect(try await harness.store.deliveries(meetingID: meeting.id).count == 1)

    var iterator = events.makeAsyncIterator()
    #expect(
      await iterator.next() == .progress(meetingID: meeting.id, stage: .summarize, fraction: 0.6))
    #expect(
      await iterator.next() == .progress(meetingID: meeting.id, stage: .deliver, fraction: 0.8))
    #expect(
      await iterator.next() == .progress(meetingID: meeting.id, stage: .deliver, fraction: 0.8))

    await #expect(throws: PipelineFailure.self) {
      try await harness.pipeline.redeliver(meetingID: SampleData.uuid(999))
    }
    await #expect(throws: PipelineFailure.self) {
      try await harness.pipeline.rerunSummary(
        meetingID: SampleData.uuid(999), templateID: "default")
    }
  }

  @Test func recordingIntakeEnqueuesThroughTheRealPipeline() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    try await harness.store.save(
      SampleData.pairedDevice(), tokenHash: Data(repeating: 1, count: 32))
    let upload = harness.directory.appendingPathComponent("upload.wav")
    try FileManager.default.copyItem(
      at: Fixtures.url("audio/conversation-two-lane-6s.wav"), to: upload)
    var metadata = SampleData.recordingMetadata()
    metadata.format = .wav16kInt16
    let intake = RecordingIntake(
      store: harness.store, settings: harness.settingsStore, pipeline: harness.pipeline,
      now: { PipelineHarness.now })
    let meetingID = try await intake.admit(
      file: upload, metadata: metadata, device: SampleData.pairedDevice())
    await harness.pipeline.waitUntilIdle()
    let export = try await harness.store.export(meetingID: meetingID)
    #expect(export.meeting.state == .ready)
    #expect(export.meeting.source == .phone)
    #expect(export.segments.count == 6)
    #expect(
      try await harness.store.receipt(metadata.recordingID)?.state
        == .complete(meetingID: meetingID))
  }

  @Test func cleanupFailureKeepsTheMergedTranscriptAndStopsTheEvents() async throws {
    struct Boom: Error {}
    let harness = try await PipelineHarness(cleaner: PassthroughCleaner(failure: Boom()))
    defer { harness.cleanUp() }
    let events = await harness.events.subscribe()
    let (meeting, asset) = harness.meeting(source: .macCall)
    try await harness.store.save(meeting, asset: asset)
    let error = await #expect(throws: PipelineFailure.self) {
      try await harness.pipeline.process(assetID: asset.id)
    }
    #expect(error == PipelineFailure(stage: .cleanup, reason: "Boom()"))
    let export = try await harness.store.export(meetingID: meeting.id)
    #expect(export.meeting.state == .failed(reason: "cleanup: Boom()"))
    #expect(export.segments.count == 12)
    #expect(export.segments.allSatisfy { $0.text == $0.rawText })
    #expect(export.speakers.map(\.clusterLabel) == ["Me", "Speaker 1", "Speaker 2"])
    #expect(export.meeting.summary == nil)
    #expect(export.audio?.mixdownURL == nil)
    #expect(export.audio?.expiresAt == nil)
    #expect(await harness.dispatcher.calls.count == 0)

    let sentinel = MeetingEvent.speakersNeedReview(meetingID: meeting.id, speakerIDs: [])
    await harness.events.post(sentinel)
    var iterator = events.makeAsyncIterator()
    var stages: [PipelineStage] = []
    while let event = await iterator.next(), event != sentinel {
      if case .progress(_, let stage, _) = event { stages.append(stage) }
    }
    #expect(stages == [.decode, .transcribe, .diarize, .matchSpeakers, .merge, .cleanup])
  }

  @Test func processingAnUnknownAssetFailsAtDecode() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let error = await #expect(throws: PipelineFailure.self) {
      try await harness.pipeline.process(assetID: SampleData.uuid(999))
    }
    #expect(error?.stage == .decode)
    #expect(error?.reason.contains("not found") == true)
    #expect(try await harness.store.meetings().isEmpty)
  }

  @Test func rerunSummaryFailureMarksFailedAndKeepsThePreviousSummary() async throws {
    struct Boom: Error {}
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, asset) = harness.meeting(source: .macInPerson)
    try await harness.pipeline.enqueue(meeting, asset: asset)
    await harness.pipeline.waitUntilIdle()
    let before = try await harness.store.export(meetingID: meeting.id)
    #expect(before.meeting.state == .ready)

    let failing = try await PipelineHarness(
      summarizer: FakeSummarizer(failure: Boom()), sharedStore: harness.store)
    defer { failing.cleanUp() }
    let error = await #expect(throws: PipelineFailure.self) {
      try await failing.pipeline.rerunSummary(meetingID: meeting.id, templateID: "interview")
    }
    #expect(error == PipelineFailure(stage: .summarize, reason: "Boom()"))
    let after = try await harness.store.export(meetingID: meeting.id)
    #expect(after.meeting.state == .failed(reason: "summarize: Boom()"))
    #expect(after.meeting.summary == before.meeting.summary)
    #expect(after.meeting.templateID == "default")
    #expect(after.segments == before.segments)
    #expect(after.tasks == before.tasks)
    #expect(after.decisions == before.decisions)
    #expect(await failing.dispatcher.calls.count == 0)
  }

  @Test func redeliverHandsTheStoredReceiptToTheDestination() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, asset) = harness.meeting(source: .macInPerson)
    try await harness.pipeline.enqueue(meeting, asset: asset)
    await harness.pipeline.waitUntilIdle()
    var delivery = try #require(try await harness.store.deliveries(meetingID: meeting.id).first)
    var receipt = try #require(delivery.receipt)
    receipt.folder = "moved-by-the-user"
    delivery.receipt = receipt
    try await harness.store.save(delivery)

    try await harness.pipeline.redeliver(meetingID: meeting.id)

    let deliveries = try await harness.store.deliveries(meetingID: meeting.id)
    #expect(deliveries.map(\.id) == [delivery.id])
    #expect(deliveries.first?.status == .delivered)
    #expect(deliveries.first?.receipt?.folder == "moved-by-the-user")
    let moved = harness.destination.root.appendingPathComponent("moved-by-the-user/meeting.json")
    let written = try Data(contentsOf: moved)
    #expect(deliveries.first?.receipt?.files.first?.sha256 == ContentHash.sha256(written))
    #expect(try StenoJSON.decode(MeetingExport.self, from: written).meeting.id == meeting.id)
    #expect(await harness.destination.deliveries.count == 2)
  }

  @Test func retentionZeroSweepRemovesTheAudioAndKeepsTheSampleClips() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let master = harness.directory.appendingPathComponent("master.wav")
    let mic = harness.directory.appendingPathComponent("mic.wav")
    let system = harness.directory.appendingPathComponent("system.wav")
    try FileManager.default.copyItem(
      at: Fixtures.url("audio/conversation-two-lane-6s.wav"), to: master)
    try FileManager.default.copyItem(at: Fixtures.url("audio/conversation-mic-6s.wav"), to: mic)
    try FileManager.default.copyItem(
      at: Fixtures.url("audio/conversation-system-6s.wav"), to: system)
    let (meeting, template) = harness.meeting(source: .macCall, retention: .deleteAfterProcessing)
    var asset = template
    asset.url = master
    asset.sidecars16k = [.mic: mic, .system: system]
    try await harness.pipeline.enqueue(meeting, asset: asset)
    await harness.pipeline.waitUntilIdle()
    let before = try await harness.store.export(meetingID: meeting.id)
    #expect(before.meeting.state == .ready)
    #expect(before.audio?.expiresAt == PipelineHarness.now)
    let mixdown = try #require(before.audio?.mixdownURL)
    let clips = before.speakers.compactMap(\.sampleClipURL)
    #expect(clips.count == 2)

    let sweep = RetentionSweep(store: harness.store)
    #expect(try await sweep.run(now: PipelineHarness.now.addingTimeInterval(-1)).isEmpty)
    let removed = try await sweep.run(now: PipelineHarness.now)
    #expect(removed == [master, mic, system, mixdown])
    for url in removed {
      #expect(!FileManager.default.fileExists(atPath: url.path), "\(url.lastPathComponent)")
    }
    for clip in clips {
      #expect(FileManager.default.fileExists(atPath: clip.path), "\(clip.lastPathComponent)")
    }
    let after = try await harness.store.export(meetingID: meeting.id)
    #expect(after.audio?.expiresAt == nil)
    #expect(after.audio?.url == master, "the row keeps its URLs")
    #expect(after.speakers.compactMap(\.sampleClipURL) == clips)
    #expect(after.segments == before.segments)
    #expect(try await sweep.run(now: .distantFuture).isEmpty)
  }

  @Test func aPhoneAACRecordingIsNotMixedDownAgain() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    try await harness.store.save(
      SampleData.pairedDevice(), tokenHash: Data(repeating: 1, count: 32))
    let upload = harness.directory.appendingPathComponent("upload.m4a")
    try FileManager.default.copyItem(
      at: Fixtures.url("audio/conversation-two-lane-6s.wav"), to: upload)
    let intake = RecordingIntake(
      store: harness.store, settings: harness.settingsStore, pipeline: harness.pipeline,
      now: { PipelineHarness.now })
    let meetingID = try await intake.admit(
      file: upload, metadata: SampleData.recordingMetadata(), device: SampleData.pairedDevice())
    await harness.pipeline.waitUntilIdle()
    let export = try await harness.store.export(meetingID: meetingID)
    #expect(export.meeting.state == .ready)
    let audio = try #require(export.audio)
    #expect(audio.format == .m4aAAC)
    #expect(audio.mixdownURL == nil)
    let folder = ProcessingPipeline.meetingFolder(meetingID, settings: harness.settings)
    #expect(audio.url.path == folder.appendingPathComponent("recording.m4a").path)
    #expect(
      !FileManager.default.fileExists(atPath: folder.appendingPathComponent("audio.m4a").path))
    #expect(audio.expiresAt == PipelineHarness.now.addingTimeInterval(30 * 86_400))
    #expect(export.speakers.allSatisfy { $0.sampleClipURL != nil })
  }
}
