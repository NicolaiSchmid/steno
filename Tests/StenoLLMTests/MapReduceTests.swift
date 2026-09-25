import Foundation
import StenoCore
import Testing

@testable import StenoLLM

/// Path selection by context size on the 60-minute fixture: one call at
/// 32k, map then reduce at 8k, `transcriptTooLong` before any call at 4k.
@Suite struct MapReduceTests {
  static let call = LLMFixtures.customerCall60min()
  static let utc = TimeZone(identifier: "UTC")!

  static func input() -> SummaryInput {
    SummaryInput(export: call, template: SummaryTemplate.bundled(id: "default"))
  }

  static func summarizer(_ server: StubChatServer, contextTokens: Int) -> LLMMeetingSummarizer {
    let endpoint = LLMEndpoint(
      baseURL: server.baseURL, model: "stub-model", contextTokens: contextTokens)
    let client = OpenAICompatibleClient(endpoint: endpoint, apiKey: nil, retry: .none)
    return LLMMeetingSummarizer(model: client, endpoint: endpoint, timeZone: utc)
  }

  /// Notes for map requests, the canned draft for everything else.
  static func responder() throws -> @Sendable (RecordedRequest) -> StubResponse? {
    let notes = try String(
      contentsOf: Fixtures.url("llm/responses/notes-customer-call.json"), encoding: .utf8)
    let draft = try String(
      contentsOf: Fixtures.url("llm/responses/summary-default-standup.json"), encoding: .utf8)
    return { request in
      let usage = LLMUsage(promptTokens: 1_000, completionTokens: 100, requests: 1)
      return Scripts.completion(request.purpose == "summary-map" ? notes : draft, usage: usage)
    }
  }

  @Test func at8kContextIssuesMapCallsThenOneReduce() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    server.respond(with: try Self.responder())
    let summarizer = Self.summarizer(server, contextTokens: 8_000)
    let output = try await summarizer.summarize(Self.input())

    let purposes = server.requests.map(\.purpose)
    let mapCount = purposes.filter { $0 == "summary-map" }.count
    #expect(mapCount >= 6 && mapCount <= 12, "\(mapCount) map calls")
    #expect(purposes.last == "summary-reduce")
    #expect(purposes.dropLast().allSatisfy { $0 == "summary-map" })
    #expect(server.requests.count == mapCount + 1)
    #expect(server.maxInFlight <= 2)

    let reduce = try #require(server.requests.last?.chat)
    let user = reduce.messages[1].content
    #expect(user.hasPrefix("Notes from \(mapCount) parts, in order (JSON):"))
    for index in 0..<mapCount {
      #expect(user.contains("\"chunkIndex\" : \(index)"), "chunk \(index) in the reduce input")
    }
    #expect(reduce.responseFormat?.jsonSchema?.name == "meeting_analysis")
    #expect(reduce.maxTokens == 2_000, "a quarter of 8k")
    // Two map calls are in flight at once, so arrival order is not chunk
    // order; every part must have been asked for exactly once.
    let maps = server.requests.dropLast().compactMap(\.chat)
    #expect(maps.allSatisfy { $0.responseFormat?.jsonSchema?.name == "chunk_notes" })
    // Each map call may spend the chunk's share of the input budget on its
    // notes, not the 1 500 ceiling: at 8k the ten chunks share about 4 800.
    let budget = summarizer.budget(for: Self.input())
    let notesTokens = budget.mapNotesOutputTokens(chunkCount: mapCount)
    #expect(notesTokens >= 256 && notesTokens < 1_500, "\(notesTokens)")
    #expect(notesTokens * mapCount <= budget.inputBudget)
    #expect(maps.allSatisfy { $0.maxTokens == notesTokens }, "\(maps.map(\.maxTokens))")
    let rule = "at most \(SummaryPromptBuilder.maxNotesPoints(for: notesTokens)) points in total"
    #expect(maps.allSatisfy { $0.messages[0].content.contains(rule) }, "\(rule)")
    for part in 1...mapCount {
      #expect(
        maps.filter { $0.messages[0].content.contains("part \(part) of \(mapCount)") }.count == 1,
        "part \(part)")
    }

    #expect(
      output.usage
        == LLMUsage(
          promptTokens: 1_000 * (mapCount + 1), completionTokens: 100 * (mapCount + 1),
          requests: mapCount + 1))
    #expect(output.summary.sections.first?.id == "executive-summary")
    #expect(output.title == "Daily Standup: Onboarding, CI und Obsidian-Export")
  }

  @Test func at32kContextOneCallSuffices() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    server.respond(with: try Self.responder())
    let output = try await Self.summarizer(server, contextTokens: 32_000).summarize(Self.input())
    #expect(server.requests.map(\.purpose) == ["summary"])
    #expect(server.requests.first?.chat?.maxTokens == 4_096)
    #expect(output.usage.requests == 1)
    let user = try #require(server.requests.first?.chat?.messages[1].content)
    #expect(user.hasPrefix("Transcript:\n"))
    #expect(user.split(separator: "\n").count == Self.call.segments.count + 1)
  }

  @Test func at4kContextThrowsTranscriptTooLongBeforeAnyCall() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    server.respond(with: try Self.responder())
    let error = await #expect(throws: LLMError.self) {
      try await Self.summarizer(server, contextTokens: 4_000).summarize(Self.input())
    }
    guard case .transcriptTooLong(let estimated, let budget) = error else {
      Issue.record("expected transcriptTooLong, got \(String(describing: error))")
      return
    }
    #expect(estimated > 12_000)
    #expect(budget < 2_500)
    #expect(server.requests.isEmpty, "the transcript is untouched and nothing was sent")
  }

  @Test func aFailedMapCallPropagates() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    server.respond { _ in Scripts.serverError() }
    let error = await #expect(throws: LLMError.self) {
      try await Self.summarizer(server, contextTokens: 8_000).summarize(Self.input())
    }
    #expect(error == .http(status: 500, body: "The server had an error"))
  }

  @Test func mapAndReducePromptsMatchTheirGoldens() throws {
    let input = SummaryInput(
      export: LLMFixtures.denglishStandup(), template: SummaryTemplate.bundled(id: "default"))
    let builder = SummaryPromptBuilder(template: input.template, timeZone: Self.utc)
    let chunks = TranscriptChunker(targetTokens: 150, maxTokens: 220).chunk(
      input.segments, language: "de")
    let map = builder.buildMap(input, chunk: chunks[1], of: chunks.count, notesTokens: 1_500)
    try Snapshot.assert(
      PromptSnapshotTests.render(map), matches: "llm/prompts/summary-map-default.txt")
    #expect(map.messages[1].content.contains("End of the previous part, for context only:"))
    #expect(map.messages[0].content.contains("with chunkIndex 1:"))

    let notes = [
      ChunkNotes(
        chunkIndex: 0,
        topics: [
          .init(
            topic: "Onboarding",
            points: ["Speaker 2 hat den Pull Request für das Onboarding gemerged."])
        ],
        decisions: [], taskCandidates: [],
        speakerCues: [
          .init(
            speakerLabel: "Speaker 2", name: "Nicolai", confidence: 0.5,
            evidence: "nicolai, willst du starten?")
        ]),
      ChunkNotes(
        chunkIndex: 1,
        topics: [
          .init(topic: "Obsidian-Export", points: ["Die Firma Müller will den Export bis Freitag."])
        ],
        decisions: ["Der Export wird bis Freitag umgesetzt."],
        taskCandidates: [
          .init(
            text: "Export nach Obsidian umsetzen.", assignee: "Speaker 2", priority: .high,
            dueDate: "2026-09-25")
        ],
        speakerCues: []),
    ]
    let reduce = builder.buildReduce(input, notes: notes)
    try Snapshot.assert(
      PromptSnapshotTests.render(reduce), matches: "llm/prompts/summary-reduce-default.txt")
    #expect(reduce.messages[1].content.contains("\"chunkIndex\" : 1"))
    #expect(reduce.purpose == "summary-reduce")

    let repair = SummaryPromptBuilder.buildRepair(
      for: builder.buildSingleShot(input), schema: builder.draftSchema, invalid: "{\"title\": ",
      error: "malformed JSON at root")
    try Snapshot.assert(
      PromptSnapshotTests.render(repair), matches: "llm/prompts/summary-repair.txt")
    var sized = map
    sized.maxTokens = 1_500
    let mapRepair = SummaryPromptBuilder.buildRepair(
      for: sized, schema: builder.notesSchema, invalid: "{", error: "x")
    #expect(mapRepair.purpose == "summary-map-repair")
    #expect(mapRepair.maxTokens == 1_500)
    #expect(mapRepair.responseFormat == sized.responseFormat)
    #expect(mapRepair.messages[0].content.contains("\"chunkIndex\": integer"))
  }

  @Test func mapBoundedKeepsOrderAndPropagatesTheFirstError() async throws {
    let doubled = try await mapBounded([3, 1, 2], limit: 2) { $0 * 2 }
    #expect(doubled == [6, 2, 4])
    let empty: [Int] = try await mapBounded([], limit: 2) { (value: Int) in value }
    #expect(empty.isEmpty)
    struct Boom: Error {}
    await #expect(throws: Boom.self) {
      try await mapBounded([1, 2, 3], limit: 1) { value -> Int in
        if value == 2 { throw Boom() }
        return value
      }
    }
  }
}
