import Foundation
import Testing

@testable import StenoCore

@Suite struct FakesTests {
  @Test func fakeSpeechEngineEmitsOneSegmentPerSecond() async throws {
    let engine = FakeSpeechEngine(textPrefix: "mic")
    let buffer = AudioBuffer16k(samples: [Float](repeating: 0, count: 40_000))
    let segments = try await engine.transcribe(buffer, hint: LanguageTag(rawValue: "en").language)
    #expect(segments.count == 3)
    #expect(segments.map(\.text) == ["mic segment 1", "mic segment 2", "mic segment 3"])
    #expect(segments.last?.end == 2.5)
    #expect(segments.allSatisfy { $0.language == LanguageTag(rawValue: "de") })
    #expect(
      await engine.transcriptions.entries == [
        .init(duration: 2.5, hint: LanguageTag(rawValue: "en").language)
      ])
    try await engine.prepare()
    #expect(await engine.preparations.count == 1)
    let timed = FakeSpeechEngine.segments(
      duration: 1, segmentSeconds: 1, language: nil, textPrefix: "x", wordTimings: true)
    #expect(timed.first?.wordTimings?.count == 3)
  }

  @Test func fakeDiarizerAlternatesClusters() async throws {
    let diarizer = FakeDiarizer(clusterCount: 2, turnSeconds: 1.5)
    let result = try await diarizer.diarize(
      AudioBuffer16k(samples: [Float](repeating: 0, count: 96_000)))
    #expect(result.clusters.map(\.label) == ["Speaker 1", "Speaker 2"])
    #expect(result.clusters[0].ranges == [0...1.5, 3...4.5])
    #expect(result.clusters[1].ranges == [1.5...3, 4.5...6])
    #expect(result.clusters[0].sampleClipRange == 0...1.5)
    #expect(result.clusters[0].embedding == SampleData.embedding(axis: 0))
    let canned = FakeDiarizer(result: { _ in SampleData.diarization() })
    #expect(try await canned.diarize(AudioBuffer16k(samples: [])) == SampleData.diarization())
  }

  @Test func fakeLanguageModelAnswersFromItsQueue() async throws {
    let model = FakeLanguageModel(responses: [SampleData.llmResponse()])
    #expect(try await model.complete(SampleData.llmRequest()) == SampleData.llmResponse())
    await #expect(throws: FakeLanguageModel.Exhausted()) {
      _ = try await model.complete(SampleData.llmRequest())
    }
    #expect(await model.requests.count == 2)
  }

  @Test func passthroughCleanerAndFakeSummarizer() async throws {
    let cleaner = PassthroughCleaner()
    let input = CleanupInput(
      segments: SampleData.segments(), language: nil, participants: [], speakers: [],
      knownPeople: [])
    let output = try await cleaner.clean(input)
    #expect(output.segments == SampleData.segments())
    #expect(output.usage == LLMUsage(promptTokens: 100, completionTokens: 50, requests: 1))

    let summarizer = FakeSummarizer()
    let summary = try await summarizer.summarize(
      SummaryInput(
        meeting: SampleData.meeting(), segments: SampleData.segments(),
        speakers: SampleData.speakers(),
        participants: [], knownPeople: [], template: SampleData.template()))
    #expect(summary.title == "Summary of Produktstrategie 90/10")
    #expect(summary.summary.sections.map(\.id) == ["executive-summary"])
    #expect(summary.summary.sections.first?.bullets.first?.lead == "Speaker 1")
    #expect(summary.speakerNames.count == 2)
    #expect(await summarizer.summaries.entries == ["default"])

    struct Boom: Error {}
    let failing = FakeSummarizer(failure: Boom())
    await #expect(throws: Boom.self) {
      _ = try await failing.summarize(
        SummaryInput(
          meeting: SampleData.meeting(), segments: [], speakers: [], participants: [],
          knownPeople: [], template: SampleData.template()))
    }
  }

  @Test func recordingDestinationAndDispatcherWriteMeetingJSON() async throws {
    let root = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: root) }
    let store = try await MeetingStoreTests.populated()
    let destination = FakeDestination(root: root)
    let dispatcher = FakeDeliveryDispatcher(
      store: store, destinations: [destination], now: { SampleData.updatedAt })

    let deliveries = await dispatcher.deliverAll(meetingID: SampleData.meetingID)
    #expect(deliveries.count == 1)
    #expect(deliveries.first?.status == .delivered)
    #expect(deliveries.first?.receipt?.folder == SampleData.meetingID.uuidString)
    #expect(deliveries.first?.receipt?.files.map(\.relativePath) == ["meeting.json"])
    let data = try Data(contentsOf: destination.exportURL(meetingID: SampleData.meetingID))
    #expect(try StenoJSON.decode(MeetingExport.self, from: data).meeting.id == SampleData.meetingID)
    #expect(deliveries.first?.receipt?.files.first?.sha256 == ContentHash.sha256(data))
    #expect(try await store.deliveries(meetingID: SampleData.meetingID) == deliveries)
    #expect(await destination.deliveries.count == 1)

    let again = await dispatcher.deliverAll(meetingID: SampleData.meetingID)
    #expect(again.first?.id == deliveries.first?.id)
    #expect(try await store.deliveries(meetingID: SampleData.meetingID).count == 1)

    struct Boom: Error {}
    let failing = FakeDeliveryDispatcher(
      store: store,
      destinations: [FakeDestination(id: "broken", root: root, deliverFailure: Boom())])
    let failed = await failing.deliverAll(meetingID: SampleData.meetingID)
    #expect(failed.first?.status == .failed("Boom()"))
    #expect(await dispatcher.deliverAll(meetingID: SampleData.uuid(999)).isEmpty)
  }

  @Test func fakeHandoverIntakeRecordsAdmissions() async throws {
    let intake = FakeHandoverIntake(meetingID: SampleData.meetingID)
    let id = try await intake.admit(
      file: URL(fileURLWithPath: "/tmp/x.m4a"), metadata: SampleData.recordingMetadata(),
      device: SampleData.pairedDevice())
    #expect(id == SampleData.meetingID)
    #expect(await intake.admissions.count == 1)
  }

  @Test func transcriptFixturesMatchTheirGoldens() throws {
    try Snapshot.assert(
      try StenoJSON.encode(SampleData.rawSegments()), matches: "transcripts/raw-segments.json")
    try Snapshot.assert(
      try StenoJSON.encode(SampleData.segments()), matches: "transcripts/segments.json")
  }
}
