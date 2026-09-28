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
    let (meeting, asset) = try observedHarness.meeting(source: .macCall)
    try await observedHarness.pipeline.enqueue(meeting, asset: asset)
    await observedHarness.pipeline.waitUntilIdle()

    let export = try await observedHarness.store.export(meetingID: meeting.id)
    #expect(export.meeting.state == .ready)
    #expect(export.meeting.title == "Summary of Untitled")
    #expect(export.meeting.language == LanguageTag(rawValue: "de"))
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
    #expect(audio.mixdownURL == RecordingLayout(asset: asset).mixdown(.wav16kInt16))
    #expect(FileManager.default.fileExists(atPath: try #require(audio.mixdownURL).path))
    #expect(audio.expiresAt == PipelineHarness.now.addingTimeInterval(30 * 86_400))
    #expect(
      export.speakers.filter { $0.clusterLabel != "Me" }.allSatisfy { $0.sampleClipURL != nil })

    #expect(await observedHarness.dispatcher.dispatches.entries == [meeting.id])
    let deliveries = try await observedHarness.store.deliveries(meetingID: meeting.id)
    #expect(deliveries.map(\.status) == [.delivered])
    let written = try Data(contentsOf: observedHarness.destination.exportURL(meetingID: meeting.id))
    #expect(try StenoJSON.decode(MeetingExport.self, from: written).meeting.state == .ready)

    #expect(await seen.states == [.processing])

    let collected = await observedHarness.events.drain(events)
    try #require(
      collected.count == 12, "ten stage starts, one review request, one retention applied")
    let stages = collected.compactMap { event -> PipelineStage? in
      if case .progress(_, let stage) = event { return stage }
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
    #expect(
      collected.suffix(2) == [
        .progress(meetingID: meeting.id, stage: .retention),
        .retentionApplied(meetingID: meeting.id),
      ], "the sweep trigger follows the expiry write and is the last event of a run")
  }

  @Test func observeMeetingSeesTheStatesInOrder() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, asset) = try harness.meeting(source: .macInPerson)
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
    let (meeting, asset) = try harness.meeting(source: .macInPerson)
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
    let (meeting, asset) = try harness.meeting(source: .macCall)
    try await harness.store.save(meeting, asset: asset)
    let error = await #expect(throws: PipelineFailure.self) {
      try await harness.pipeline.process(assetID: asset.id)
    }
    #expect(error?.stage == .summarize)
    let stored = try #require(try await harness.store.meeting(id: meeting.id))
    #expect(stored.state == .failed(reason: "summarize: Boom()"))
    #expect(try await harness.store.export(meetingID: meeting.id).segments.count == 12)
    #expect(await harness.dispatcher.dispatches.count == 0)
    #expect(stored.summary == nil)
  }

  /// Decision 2 of the onboarding plan: `.ready` with `summary == nil` means
  /// the summary was skipped for lack of an endpoint. Nil passes post their
  /// progress, write nothing, and the meeting still lands ready and
  /// delivered with the merged transcript and its original title.
  @Test func withoutAnEndpointProcessSkipsBothPassesAndLandsReady() async throws {
    let harness = try await PipelineHarness(cleaner: nil, summarizer: nil)
    defer { harness.cleanUp() }
    let events = await harness.events.subscribe()
    let (meeting, asset) = try harness.meeting(source: .macCall)
    try await harness.pipeline.enqueue(meeting, asset: asset)
    await harness.pipeline.waitUntilIdle()

    let export = try await harness.store.export(meetingID: meeting.id)
    #expect(export.meeting.state == .ready)
    #expect(export.meeting.summary == nil)
    #expect(export.meeting.llmUsage == nil)
    #expect(export.meeting.title == "Untitled")
    #expect(
      export.meeting.language == LanguageTag(rawValue: "de"), "the transcript's, not a model's")
    #expect(export.tasks.isEmpty)
    #expect(export.decisions.isEmpty)
    #expect(try await harness.store.nameSuggestions(meetingID: meeting.id).isEmpty)
    #expect(export.segments.count == 12)
    #expect(export.segments.allSatisfy { $0.text == $0.rawText })
    #expect(export.audio?.mixdownURL != nil)
    #expect(await harness.dispatcher.dispatches.entries == [meeting.id])
    #expect(
      try await harness.store.deliveries(meetingID: meeting.id).map(\.status) == [.delivered])

    let stages = await harness.events.drain(events).compactMap { event -> PipelineStage? in
      if case .progress(_, let stage) = event { return stage }
      return nil
    }
    #expect(stages == PipelineStage.allCases, "cleanup and summarize still report progress")

    let error = await #expect(throws: PipelineFailure.self) {
      try await harness.pipeline.rerunSummary(meetingID: meeting.id, templateID: "interview")
    }
    #expect(error?.stage == .summarize)
    #expect(error?.reason.contains("LLM endpoint") == true)
    let after = try await harness.store.export(meetingID: meeting.id)
    #expect(after == export, "a refused re-run changes nothing")
    #expect(await harness.dispatcher.dispatches.count == 1)

    try await harness.pipeline.redeliver(meetingID: meeting.id)
    #expect(await harness.dispatcher.dispatches.count == 2)
  }

  /// The other side of the invariant: a configured summarizer never leaves a
  /// ready meeting without a summary, even when the model has nothing to say.
  @Test func aConfiguredSummarizerNeverLeavesReadyWithoutASummary() async throws {
    let empty = SummaryOutput(
      title: "", summary: SummaryDocument(templateID: "default", sections: []), decisions: [],
      tasks: [], speakerNames: [], usage: .zero)
    // No cleaner, so only the summarize stage can make `llmUsage` non-nil.
    let harness = try await PipelineHarness(
      cleaner: nil, summarizer: FakeSummarizer(canned: empty))
    defer { harness.cleanUp() }
    let (meeting, asset) = try harness.meeting(source: .macInPerson)
    try await harness.pipeline.enqueue(meeting, asset: asset)
    await harness.pipeline.waitUntilIdle()

    let export = try await harness.store.export(meetingID: meeting.id)
    #expect(export.meeting.state == .ready)
    #expect(export.meeting.summary != nil)
    #expect(export.meeting.summary?.sections.isEmpty == true)
    #expect(export.meeting.llmUsage == .zero, "the summarizer ran and reported its usage")
    #expect(export.meeting.title == "Untitled")
  }

  /// A `.processing` meeting picked up at launch after the endpoint was
  /// removed: the earlier run's summary, rows and usage are replaced, not
  /// kept or added to.
  @Test func resumeWithNilPassesClearsAnEarlierRunsSummaryAndUsage() async throws {
    let harness = try await PipelineHarness(cleaner: nil, summarizer: nil)
    defer { harness.cleanUp() }
    let earlier = SampleData.summaryOutput()
    let (fresh, asset) = try harness.meeting(source: .macInPerson)
    var meeting = fresh
    meeting.state = .processing
    meeting.summary = earlier.summary
    meeting.llmUsage = LLMUsage(promptTokens: 1, completionTokens: 1, requests: 1)
    try await harness.store.save(meeting, asset: asset)
    try await harness.store.replaceSummary(
      meeting, tasks: earlier.tasks, decisions: earlier.decisions,
      speakerNames: earlier.speakerNames)
    #expect(try await harness.store.export(meetingID: meeting.id).tasks.isEmpty == false)

    #expect(try await harness.pipeline.resumeUnfinished() == [meeting.id])
    await harness.pipeline.waitUntilIdle()

    let export = try await harness.store.export(meetingID: meeting.id)
    #expect(export.meeting.state == .ready)
    #expect(export.meeting.summary == nil)
    #expect(export.meeting.llmUsage == nil)
    #expect(export.tasks.isEmpty)
    #expect(export.decisions.isEmpty)
    #expect(try await harness.store.nameSuggestions(meetingID: meeting.id).isEmpty)
  }

  @Test func retentionZeroExpiresImmediately() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, asset) = try harness.meeting(
      source: .macInPerson, retention: .deleteAfterProcessing)
    try await harness.pipeline.enqueue(meeting, asset: asset)
    await harness.pipeline.waitUntilIdle()
    let stored = try #require(try await harness.store.asset(id: asset.id))
    #expect(stored.expiresAt == PipelineHarness.now)
    let forever = try await PipelineHarness()
    defer { forever.cleanUp() }
    let (m2, a2) = try forever.meeting(source: .macInPerson, retention: .keepForever)
    try await forever.pipeline.enqueue(m2, asset: a2)
    await forever.pipeline.waitUntilIdle()
    #expect(try await forever.store.asset(id: a2.id)?.expiresAt == nil)
  }

  @Test func rerunSummaryAndRedeliver() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, asset) = try harness.meeting(source: .macInPerson)
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
    #expect(await harness.summarizer?.summaries.entries == ["default", "daily-standup"])
    #expect(await harness.dispatcher.dispatches.count == 2)

    try await harness.pipeline.redeliver(meetingID: meeting.id)
    #expect(await harness.dispatcher.dispatches.count == 3)
    #expect(try await harness.store.deliveries(meetingID: meeting.id).count == 1)

    var iterator = events.makeAsyncIterator()
    #expect(
      await iterator.next() == .progress(meetingID: meeting.id, stage: .summarize))
    #expect(
      await iterator.next() == .progress(meetingID: meeting.id, stage: .deliver))
    #expect(
      await iterator.next() == .progress(meetingID: meeting.id, stage: .deliver))

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
      try await harness.store.handoverReceipt(recordingID: metadata.recordingID)?.state
        == .complete(meetingID: meetingID))
  }

  @Test func cleanupFailureKeepsTheMergedTranscriptAndStopsTheEvents() async throws {
    struct Boom: Error {}
    let harness = try await PipelineHarness(cleaner: PassthroughCleaner(failure: Boom()))
    defer { harness.cleanUp() }
    let events = await harness.events.subscribe()
    let (meeting, asset) = try harness.meeting(source: .macCall)
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
    #expect(await harness.dispatcher.dispatches.count == 0)

    let stages = await harness.events.drain(events).compactMap { event -> PipelineStage? in
      if case .progress(_, let stage) = event { return stage }
      return nil
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

  @Test func rerunSummaryFailureIsThrownAndLeavesTheReadyMeetingAlone() async throws {
    struct Boom: Error {}
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, asset) = try harness.meeting(source: .macInPerson)
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
    #expect(after == before, "state, summary, template, transcript, tasks and decisions are kept")
    #expect(await failing.dispatcher.dispatches.count == 0)
  }

  @Test func editsMadeWhileTheMeetingIsProcessingSurvive() async throws {
    let store = try MeetingStore.inMemory()
    var diarizer = FakeDiarizer()
    diarizer.onDiarize = {
      // The user types notes and tags while STT runs; the app saves the row.
      _ = try? await store.update(meetingID: SampleData.meetingID, now: SampleData.updatedAt) {
        $0.scratchpad = "Nachfassen wegen Budget."
        $0.tags = ["strategie"]
      }
    }
    let harness = try await PipelineHarness(diarizer: diarizer, sharedStore: store)
    defer { harness.cleanUp() }
    let (meeting, asset) = try harness.meeting(source: .macCall)
    try await harness.pipeline.enqueue(meeting, asset: asset)
    await harness.pipeline.waitUntilIdle()

    let stored = try #require(try await store.meeting(id: meeting.id))
    #expect(stored.state == .ready)
    #expect(stored.scratchpad == "Nachfassen wegen Budget.")
    #expect(stored.tags == ["strategie"])
    #expect(stored.title == "Summary of Untitled", "the pipeline's own columns still land")
    #expect(stored.summary != nil)

    // The same holds for a rerun started from a stale snapshot.
    try await store.update(meetingID: meeting.id, now: SampleData.updatedAt) {
      $0.scratchpad = "Edited during the rerun."
    }
    try await harness.pipeline.rerunSummary(meetingID: meeting.id, templateID: "interview")
    let rerun = try #require(try await store.meeting(id: meeting.id))
    #expect(rerun.scratchpad == "Edited during the rerun.")
    #expect(rerun.templateID == "interview")
  }

  @Test func aSecondOperationOnAnInFlightMeetingIsRefused() async throws {
    let store = try MeetingStore.inMemory()
    let gate = Gate()
    var diarizer = FakeDiarizer()
    diarizer.onDiarize = { await gate.wait() }
    let harness = try await PipelineHarness(diarizer: diarizer, sharedStore: store)
    defer { harness.cleanUp() }
    let (meeting, asset) = try harness.meeting(source: .macInPerson)
    try await harness.store.save(meeting, asset: asset)

    let first = Task { try await harness.pipeline.process(assetID: asset.id) }
    await gate.waitUntilBlocked()
    let second = await #expect(throws: PipelineFailure.self) {
      try await harness.pipeline.process(assetID: asset.id)
    }
    #expect(second?.reason.contains("already being processed") == true)
    await #expect(throws: PipelineFailure.self) {
      try await harness.pipeline.rerunSummary(meetingID: meeting.id, templateID: "default")
    }
    await #expect(throws: PipelineFailure.self) {
      try await harness.pipeline.redeliver(meetingID: meeting.id)
    }
    await #expect(throws: PipelineFailure.self) {
      try await harness.pipeline.enqueue(meeting, asset: asset)
    }
    await gate.open()
    try await first.value

    #expect(await harness.engine.transcriptions.count == 1)
    #expect(await harness.summarizer?.summaries.count == 1)
    #expect(await harness.dispatcher.dispatches.count == 1)
    #expect(try await harness.store.meeting(id: meeting.id)?.state == .ready)
    // Once the run is over the meeting is free again.
    try await harness.pipeline.redeliver(meetingID: meeting.id)
    #expect(await harness.dispatcher.dispatches.count == 2)
  }

  @Test func resumeUnfinishedProcessesLeftoversOldestFirstAndFailsThoseWithoutAnAsset()
    async throws
  {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    // What a process that exited mid-run leaves behind: a queued meeting, a
    // processing one that started earlier, a queued one whose asset row is
    // gone, plus a ready and a recording meeting that are none of resume's
    // business.
    let (template, templateAsset) = try harness.meeting(source: .macInPerson)
    var queued = template
    queued.state = .queued
    try await harness.store.save(queued, asset: templateAsset)

    var processing = template
    processing.id = SampleData.uuid(2)
    processing.state = .processing
    processing.startedAt = template.startedAt.addingTimeInterval(-3600)
    let layout = RecordingLayout(
      audioFolder: harness.settings.audioFolder, meetingID: processing.id)
    try layout.createDirectories()
    try FileManager.default.copyItem(at: templateAsset.url, to: layout.master(.wav16kInt16))
    var processingAsset = templateAsset
    processingAsset.id = SampleData.uuid(71)
    processingAsset.meetingID = processing.id
    processingAsset.url = layout.master(.wav16kInt16)
    try await harness.store.save(processing, asset: processingAsset)

    var orphan = template
    orphan.id = SampleData.uuid(3)
    orphan.state = .queued
    try await harness.store.save(orphan)
    var ready = template
    ready.id = SampleData.uuid(4)
    ready.state = .ready
    try await harness.store.save(ready)
    var recording = template
    recording.id = SampleData.uuid(5)
    recording.state = .recording
    try await harness.store.save(recording)

    let resumed = try await harness.pipeline.resumeUnfinished()
    #expect(resumed == [processing.id, queued.id], "oldest first")
    await harness.pipeline.waitUntilIdle()

    #expect(try await harness.store.meeting(id: queued.id)?.state == .ready)
    #expect(try await harness.store.meeting(id: processing.id)?.state == .ready)
    let orphaned = try #require(try await harness.store.meeting(id: orphan.id))
    guard case .failed(let reason) = orphaned.state else {
      Issue.record("expected .failed, got \(orphaned.state)")
      return
    }
    #expect(reason.contains("asset is missing"))
    #expect(orphaned.updatedAt == PipelineHarness.now)
    #expect(try await harness.store.meeting(id: ready.id)?.state == .ready)
    #expect(try await harness.store.meeting(id: recording.id)?.state == .recording)
    #expect(await harness.summarizer?.summaries.count == 2)
    // Both run in the background at once, so only the set is fixed.
    #expect(Set(await harness.dispatcher.dispatches.entries) == [processing.id, queued.id])
    #expect(try await harness.pipeline.resumeUnfinished().isEmpty, "nothing left to resume")
  }

  @Test func resumeUnfinishedSkipsAMeetingThatIsAlreadyInFlight() async throws {
    let store = try MeetingStore.inMemory()
    let gate = Gate()
    var diarizer = FakeDiarizer()
    diarizer.onDiarize = { await gate.wait() }
    let harness = try await PipelineHarness(diarizer: diarizer, sharedStore: store)
    defer { harness.cleanUp() }
    let (meeting, asset) = try harness.meeting(source: .macInPerson)
    try await harness.pipeline.enqueue(meeting, asset: asset)
    await gate.waitUntilBlocked()
    #expect(try await store.meeting(id: meeting.id)?.state == .processing)

    #expect(try await harness.pipeline.resumeUnfinished().isEmpty)

    await gate.open()
    await harness.pipeline.waitUntilIdle()
    #expect(try await store.meeting(id: meeting.id)?.state == .ready)
    #expect(await harness.engine.transcriptions.count == 1, "processed once")
  }

  /// Blocks one task until opened; tests use it to hold a stage mid-flight.
  actor Gate {
    private var opened = false
    private var waiting: [CheckedContinuation<Void, Never>] = []
    private var blocked: [CheckedContinuation<Void, Never>] = []

    func wait() async {
      if opened { return }
      await withCheckedContinuation { continuation in
        waiting.append(continuation)
        for observer in blocked { observer.resume() }
        blocked.removeAll()
      }
    }

    func waitUntilBlocked() async {
      if !waiting.isEmpty { return }
      await withCheckedContinuation { blocked.append($0) }
    }

    func open() {
      opened = true
      for continuation in waiting { continuation.resume() }
      waiting.removeAll()
    }
  }

  @Test func redeliverHandsTheStoredReceiptToTheDestination() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, asset) = try harness.meeting(source: .macInPerson)
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
    let (meeting, template) = try harness.meeting(
      source: .macCall, retention: .deleteAfterProcessing)
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

  /// Confirming a speaker keeps the clip while the recording exists, gives
  /// the person the speaker's voice, and naming somebody else moves it.
  @Test func confirmThenReassignKeepsTheClipAndMovesTheVoice() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, asset) = try harness.meeting(source: .macCall)
    try await harness.pipeline.enqueue(meeting, asset: asset)
    await harness.pipeline.waitUntilIdle()
    let export = try await harness.store.export(meetingID: meeting.id)
    let speaker = try #require(export.speakers.first { $0.embedding != nil })
    let clip = try #require(speaker.sampleClipURL)
    #expect(FileManager.default.fileExists(atPath: clip.path))
    let anna = Person(id: SampleData.uuid(12), displayName: "Anna", createdAt: PipelineHarness.now)
    let bea = Person(id: SampleData.uuid(13), displayName: "Bea", createdAt: PipelineHarness.now)

    try await harness.store.confirm(speakerID: speaker.id, person: anna)
    var stored = try #require(
      try await harness.store.speakers(meetingID: meeting.id)
        .first { $0.id == speaker.id })
    #expect(stored.assignment == .confirmed(personID: anna.id))
    #expect(stored.sampleClipURL == clip)
    #expect(FileManager.default.fileExists(atPath: clip.path))
    var annaRow = try #require(try await harness.store.person(id: anna.id))
    #expect(annaRow.sampleCount == 1)
    #expect(annaRow.embedding == speaker.embedding?.normalized())

    try await harness.store.confirm(speakerID: speaker.id, person: bea)
    stored = try #require(
      try await harness.store.speakers(meetingID: meeting.id)
        .first { $0.id == speaker.id })
    #expect(stored.assignment == .confirmed(personID: bea.id))
    #expect(FileManager.default.fileExists(atPath: clip.path))
    annaRow = try #require(try await harness.store.person(id: anna.id))
    #expect(annaRow.sampleCount == 0)
    #expect(annaRow.embedding == nil)
    let beaRow = try #require(try await harness.store.person(id: bea.id))
    #expect(beaRow.sampleCount == 1)
    #expect(beaRow.embedding == speaker.embedding?.normalized())
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
    let layout = RecordingLayout(audioFolder: harness.settings.audioFolder, meetingID: meetingID)
    #expect(audio.url.path == layout.master(.m4aAAC).path)
    #expect(!FileManager.default.fileExists(atPath: layout.mixdown(.m4aAAC).path))
    #expect(audio.expiresAt == PipelineHarness.now.addingTimeInterval(30 * 86_400))
    #expect(export.speakers.allSatisfy { $0.sampleClipURL != nil })
  }

  // MARK: - Deletion waits for delivery

  /// The retention stage collects what a run posted, up to a sentinel so a
  /// failed run never hangs the test.
  private func retentionEvents(_ harness: PipelineHarness, _ events: AsyncStream<MeetingEvent>)
    async -> [MeetingEvent]
  {
    await harness.events.drain(events).filter {
      if case .retentionApplied = $0 { return true }
      return false
    }
  }

  @Test func aFailedDeliveryDefersExpiryUntilRedeliverSucceeds() async throws {
    let harness = try await PipelineHarness(failDeliveriesUntil: 1)
    defer { harness.cleanUp() }
    let events = await harness.events.subscribe()
    let (meeting, asset) = try harness.meeting(
      source: .macInPerson, retention: .deleteAfterProcessing)
    try await harness.pipeline.enqueue(meeting, asset: asset)
    await harness.pipeline.waitUntilIdle()

    #expect(try await harness.store.meeting(id: meeting.id)?.state == .ready, "process succeeded")
    #expect(
      try await harness.store.deliveries(meetingID: meeting.id).map(\.status.kind) == [.failed])
    let deferred = try #require(try await harness.store.asset(id: asset.id))
    #expect(deferred.expiresAt == nil, "not stamped while the export is outstanding")
    #expect(deferred.retention == .deleteAfterProcessing)
    #expect(await retentionEvents(harness, events).isEmpty, "no sweep trigger")
    #expect(try await RetentionSweep(store: harness.store).run(now: .distantFuture).isEmpty)
    #expect(FileManager.default.fileExists(atPath: asset.url.path))

    let second = await harness.events.subscribe()
    try await harness.pipeline.redeliver(meetingID: meeting.id)
    #expect(
      try await harness.store.deliveries(meetingID: meeting.id).map(\.status) == [.delivered])
    let stamped = try #require(try await harness.store.asset(id: asset.id))
    #expect(stamped.expiresAt == PipelineHarness.now, "stamped once the export succeeded")
    #expect(
      await retentionEvents(harness, second) == [.retentionApplied(meetingID: meeting.id)])

    // A stamped asset is never restamped: a later re-export does not move
    // the expiry, and posts no second trigger.
    var moved = stamped
    moved.expiresAt = PipelineHarness.now.addingTimeInterval(-3_600)
    try await harness.store.save(moved)
    let third = await harness.events.subscribe()
    try await harness.pipeline.redeliver(meetingID: meeting.id)
    #expect(try await harness.store.asset(id: asset.id)?.expiresAt == moved.expiresAt)
    #expect(await retentionEvents(harness, third).isEmpty)
  }

  @Test func rerunSummaryAlsoStampsADeferredExpiry() async throws {
    let harness = try await PipelineHarness(failDeliveriesUntil: 1)
    defer { harness.cleanUp() }
    let (meeting, asset) = try harness.meeting(source: .macInPerson, retention: .keepDays(3))
    try await harness.pipeline.enqueue(meeting, asset: asset)
    await harness.pipeline.waitUntilIdle()
    #expect(try await harness.store.asset(id: asset.id)?.expiresAt == nil)

    let events = await harness.events.subscribe()
    try await harness.pipeline.rerunSummary(meetingID: meeting.id, templateID: "daily-standup")
    let stamped = try #require(try await harness.store.asset(id: asset.id))
    #expect(stamped.expiresAt == PipelineHarness.now.addingTimeInterval(3 * 86_400))
    #expect(stamped.retention == .keepDays(3))
    #expect(await retentionEvents(harness, events) == [.retentionApplied(meetingID: meeting.id)])
  }

  @Test func keepForeverStampsNilWhetherDeliverySucceedsOrFails() async throws {
    for failures in [0, 1] {
      let harness = try await PipelineHarness(failDeliveriesUntil: failures)
      defer { harness.cleanUp() }
      let (meeting, asset) = try harness.meeting(source: .macInPerson, retention: .keepForever)
      try await harness.pipeline.enqueue(meeting, asset: asset)
      await harness.pipeline.waitUntilIdle()
      try await harness.pipeline.redeliver(meetingID: meeting.id)
      let stored = try #require(try await harness.store.asset(id: asset.id))
      #expect(stored.retention == .keepForever, "failures=\(failures)")
      #expect(stored.expiresAt == nil, "failures=\(failures)")
      #expect(FileManager.default.fileExists(atPath: asset.url.path))
    }
  }

  /// Processing failure keeps the audio: the retention stage is only reached
  /// after `persist`, so a failed meeting stays unstamped and can be
  /// processed again from its files.
  @Test func aProcessingFailureLeavesTheAudioUnstamped() async throws {
    struct Boom: Error {}
    let harness = try await PipelineHarness(engine: FakeSpeechEngine(failure: Boom()))
    defer { harness.cleanUp() }
    let (meeting, asset) = try harness.meeting(
      source: .macInPerson, retention: .deleteAfterProcessing)
    try await harness.pipeline.enqueue(meeting, asset: asset)
    await harness.pipeline.waitUntilIdle()
    let stored = try #require(try await harness.store.meeting(id: meeting.id))
    #expect(stored.state.isFailed)
    let audio = try #require(try await harness.store.asset(id: asset.id))
    #expect(audio.expiresAt == nil)
    #expect(audio.retention == .deleteAfterProcessing)
    #expect(await harness.dispatcher.dispatches.count == 0)
    #expect(try await RetentionSweep(store: harness.store).run(now: .distantFuture).isEmpty)
    #expect(FileManager.default.fileExists(atPath: asset.url.path), "the master survives")
  }

  /// Guard rule 1's empty case: no destination, no row, `allDelivered` is
  /// true and the asset is stamped as soon as processing ends.
  @Test func aMeetingWithoutDestinationsIsStampedAtOnce() async throws {
    let harness = try await PipelineHarness(destinations: [])
    defer { harness.cleanUp() }
    let events = await harness.events.subscribe()
    let (meeting, asset) = try harness.meeting(
      source: .macInPerson, retention: .deleteAfterProcessing)
    try await harness.pipeline.enqueue(meeting, asset: asset)
    await harness.pipeline.waitUntilIdle()
    #expect(try await harness.store.deliveries(meetingID: meeting.id).isEmpty)
    #expect(try await harness.store.asset(id: asset.id)?.expiresAt == PipelineHarness.now)
    #expect(await retentionEvents(harness, events) == [.retentionApplied(meetingID: meeting.id)])
  }

  /// A failed meeting needs its audio for the next run: Re-export on it
  /// stamps nothing even when nothing is left to deliver.
  @Test func redeliverOnAFailedMeetingStampsNothing() async throws {
    struct Boom: Error {}
    let harness = try await PipelineHarness(
      engine: FakeSpeechEngine(failure: Boom()), destinations: [])
    defer { harness.cleanUp() }
    let (meeting, asset) = try harness.meeting(
      source: .macInPerson, retention: .deleteAfterProcessing)
    try await harness.pipeline.enqueue(meeting, asset: asset)
    await harness.pipeline.waitUntilIdle()
    #expect(try await harness.store.meeting(id: meeting.id)?.state.isFailed == true)

    let events = await harness.events.subscribe()
    try await harness.pipeline.redeliver(meetingID: meeting.id)
    #expect(try await harness.store.deliveries(meetingID: meeting.id).isEmpty)
    #expect(try await harness.store.asset(id: asset.id)?.expiresAt == nil)
    #expect(await retentionEvents(harness, events).isEmpty)
    #expect(try await RetentionSweep(store: harness.store).run(now: .distantFuture).isEmpty)
    #expect(FileManager.default.fileExists(atPath: asset.url.path), "the master survives")
  }

  /// The stages write the row they read at the end, not the copy `process`
  /// loaded at the start: a "Forever" chosen in Settings while the meeting
  /// was processing survives `persist` and `retention`, and a finite rule
  /// chosen meanwhile is what gets stamped.
  @Test func aRetentionChosenWhileProcessingSurvivesPersistAndRetention() async throws {
    let toForever = RetentionGate()
    let harness = try await PipelineHarness(cleaner: RetentionGatedCleaner(gate: toForever))
    defer { harness.cleanUp() }
    let events = await harness.events.subscribe()
    let (meeting, asset) = try harness.meeting(source: .macInPerson, retention: .keepDays(30))
    try await harness.pipeline.enqueue(meeting, asset: asset)
    await toForever.waitUntilEntered()
    #expect(try await harness.store.meeting(id: meeting.id)?.state == .processing)
    #expect(try await RetentionSweep(store: harness.store).keepAll() == 1)
    await toForever.open()
    await harness.pipeline.waitUntilIdle()
    let kept = try #require(try await harness.store.asset(id: asset.id))
    #expect(try await harness.store.meeting(id: meeting.id)?.state == .ready)
    #expect(kept.retention == .keepForever, "keepAll() is not overwritten by persist")
    #expect(kept.expiresAt == nil)
    #expect(kept.mixdownURL != nil, "persist still wrote the mixdown onto the re-read row")
    #expect(await retentionEvents(harness, events) == [.retentionApplied(meetingID: meeting.id)])
    #expect(try await RetentionSweep(store: harness.store).run(now: .distantFuture).isEmpty)

    let toDays = RetentionGate()
    let second = try await PipelineHarness(cleaner: RetentionGatedCleaner(gate: toDays))
    defer { second.cleanUp() }
    let (forever, foreverAsset) = try second.meeting(source: .macInPerson, retention: .keepForever)
    try await second.pipeline.enqueue(forever, asset: foreverAsset)
    await toDays.waitUntilEntered()
    try await second.pipeline.applyRetention(meetingID: forever.id, rule: .keepDays(3))
    #expect(
      try await second.store.asset(id: foreverAsset.id)?.expiresAt == nil,
      "not stamped while processing")
    await toDays.open()
    await second.pipeline.waitUntilIdle()
    let stamped = try #require(try await second.store.asset(id: foreverAsset.id))
    #expect(stamped.retention == .keepDays(3))
    #expect(stamped.expiresAt == PipelineHarness.now.addingTimeInterval(3 * 86_400))
  }

  /// The per-meeting keep: on never stamps; off stamps only under the
  /// deferred-case rules (ready meeting, every delivery succeeded), and the
  /// stamp and the post are the pipeline's.
  @Test func applyRetentionFollowsTheDeferredCaseRules() async throws {
    let harness = try await PipelineHarness(failDeliveriesUntil: 1)
    defer { harness.cleanUp() }
    let (meeting, asset) = try harness.meeting(source: .macInPerson, retention: .keepDays(7))
    try await harness.pipeline.enqueue(meeting, asset: asset)
    await harness.pipeline.waitUntilIdle()
    #expect(try await harness.store.asset(id: asset.id)?.expiresAt == nil, "export failed")

    var events = await harness.events.subscribe()
    try await harness.pipeline.applyRetention(meetingID: meeting.id, rule: .keepForever)
    var stored = try #require(try await harness.store.asset(id: asset.id))
    #expect(stored.retention == .keepForever && stored.expiresAt == nil)
    try await harness.pipeline.applyRetention(meetingID: meeting.id, rule: .keepDays(7))
    stored = try #require(try await harness.store.asset(id: asset.id))
    #expect(stored.retention == .keepDays(7))
    #expect(stored.expiresAt == nil, "off with an export outstanding stays deferred")
    #expect(await retentionEvents(harness, events).isEmpty)

    try await harness.pipeline.redeliver(meetingID: meeting.id)
    stored = try #require(try await harness.store.asset(id: asset.id))
    #expect(stored.expiresAt == PipelineHarness.now.addingTimeInterval(7 * 86_400))
    try await harness.pipeline.applyRetention(meetingID: meeting.id, rule: .keepForever)
    #expect(try await harness.store.asset(id: asset.id)?.expiresAt == nil)
    events = await harness.events.subscribe()
    try await harness.pipeline.applyRetention(meetingID: meeting.id, rule: .deleteAfterProcessing)
    stored = try #require(try await harness.store.asset(id: asset.id))
    #expect(stored.retention == .deleteAfterProcessing)
    #expect(stored.expiresAt == PipelineHarness.now, "off with every export done stamps now")
    #expect(await retentionEvents(harness, events) == [.retentionApplied(meetingID: meeting.id)])

    let failure = await #expect(throws: PipelineFailure.self) {
      try await harness.pipeline.applyRetention(meetingID: UUID(), rule: .keepForever)
    }
    #expect(failure?.stage == .retention)
  }

  /// A swept asset (stamp cleared, files gone) looks like the deferred
  /// case; a Re-export must not stamp it again and trigger another sweep.
  @Test func aSweptAssetIsNotStampedAgainOnReexport() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, asset) = try harness.meeting(
      source: .macInPerson, retention: .deleteAfterProcessing)
    try await harness.pipeline.enqueue(meeting, asset: asset)
    await harness.pipeline.waitUntilIdle()
    #expect(try await harness.store.asset(id: asset.id)?.expiresAt == PipelineHarness.now)
    #expect(!(try await RetentionSweep(store: harness.store).run(now: PipelineHarness.now)).isEmpty)
    #expect(!FileManager.default.fileExists(atPath: asset.url.path))
    #expect(try await harness.store.asset(id: asset.id)?.expiresAt == nil)

    let events = await harness.events.subscribe()
    try await harness.pipeline.redeliver(meetingID: meeting.id)
    #expect(try await harness.store.asset(id: asset.id)?.expiresAt == nil, "nothing left to delete")
    #expect(await retentionEvents(harness, events).isEmpty)
  }
}

/// Holds the pipeline inside `cleanup` until a test opened it, so the test
/// can act while the meeting is `.processing`.
private actor RetentionGate {
  private var entered = false
  private var opened = false
  private var enteredWaiters: [CheckedContinuation<Void, Never>] = []
  private var openWaiters: [CheckedContinuation<Void, Never>] = []

  /// Called by the gated stage: reports the arrival, then waits for `open()`.
  func enter() async {
    entered = true
    for waiter in enteredWaiters { waiter.resume() }
    enteredWaiters = []
    guard !opened else { return }
    await withCheckedContinuation { openWaiters.append($0) }
  }

  func open() {
    opened = true
    for waiter in openWaiters { waiter.resume() }
    openWaiters = []
  }

  func waitUntilEntered() async {
    guard !entered else { return }
    await withCheckedContinuation { enteredWaiters.append($0) }
  }
}

/// `PassthroughCleaner` behind a `RetentionGate`.
private struct RetentionGatedCleaner: TranscriptCleaner, Sendable {
  let gate: RetentionGate
  let inner = PassthroughCleaner()

  func clean(_ input: CleanupInput) async throws -> CleanupOutput {
    await gate.enter()
    return try await inner.clean(input)
  }
}
