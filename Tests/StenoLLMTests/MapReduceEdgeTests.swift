import Foundation
import StenoCore
import Testing

@testable import StenoLLM

/// Map and reduce beyond the happy path: notes that come back far too long
/// stop before the reduce call with `transcriptTooLong`, a map answer that
/// fails to decode is repaired once with the notes schema and the map
/// purpose, the model's `chunkIndex` is overruled by the chunk's, and the
/// usage of every map, repair and reduce call is summed.
@Suite struct MapReduceEdgeTests {
  static let call = LLMFixtures.customerCall60min()
  static let input = SummaryInput(export: call, template: SummaryTemplate.bundled(id: "default"))

  static func notes(_ chunkIndex: Int, points: Int = 2, pointLength: Int = 60) -> ChunkNotes {
    ChunkNotes(
      chunkIndex: chunkIndex,
      topics: [
        .init(
          topic: "Thema \(chunkIndex)",
          points: (0..<points).map { _ in String(repeating: "wort ", count: pointLength / 5) })
      ],
      decisions: [], taskCandidates: [], speakerCues: [])
  }

  @Test func oversizedNotesThrowTranscriptTooLongAfterTheMapAndBeforeTheReduce() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    let draft = try SummaryTests.canned("summary-default-standup")
    // About 6 000 estimated tokens of notes per chunk: ten chunks cannot
    // fit an 8k context however the reduce prompt is trimmed.
    server.respond { request in
      request.purpose == "summary-map"
        ? Scripts.json(Self.notes(0, points: 30, pointLength: 600)) : Scripts.completion(draft)
    }
    let summarizer = MapReduceTests.summarizer(server, contextTokens: 8_000)
    let error = await #expect(throws: LLMError.self) { try await summarizer.summarize(Self.input) }
    guard case .transcriptTooLong(let estimated, let budget) = error else {
      Issue.record("expected transcriptTooLong, got \(String(describing: error))")
      return
    }
    #expect(estimated > budget)
    let purposes = server.requests.map(\.purpose)
    #expect(purposes.allSatisfy { $0 == "summary-map" }, "no reduce call was made: \(purposes)")
    #expect(purposes.count >= 6)
  }

  @Test func aMapAnswerIsRepairedOnceWithTheNotesSchemaAndTheChunkIndexIsOverruled() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    let draft = try SummaryTests.canned("summary-default-standup")
    let mapUsage = LLMUsage(promptTokens: 500, completionTokens: 50, requests: 1)
    let repairUsage = LLMUsage(promptTokens: 700, completionTokens: 60, requests: 1)
    let reduceUsage = LLMUsage(promptTokens: 2_000, completionTokens: 400, requests: 1)
    server.respond { request in
      switch request.purpose {
      case "summary-map":
        // Part 3 answers prose the first time; every answer claims chunk 0.
        if request.chat?.messages[0].content.contains("part 3 of") == true {
          return Scripts.completion("Here are my notes, no JSON today.", usage: mapUsage)
        }
        return Scripts.json(Self.notes(0), usage: mapUsage)
      case "summary-map-repair":
        return Scripts.json(Self.notes(0), usage: repairUsage)
      default:
        return Scripts.completion(draft, usage: reduceUsage)
      }
    }
    let output = try await MapReduceTests.summarizer(server, contextTokens: 8_000).summarize(
      Self.input)
    let purposes = server.requests.map(\.purpose)
    let mapCount = purposes.filter { $0 == "summary-map" }.count
    let repairs = purposes.filter { $0 == "summary-map-repair" }
    #expect(repairs.count == 1)
    #expect(purposes.last == "summary-reduce")
    #expect(server.requests.count == mapCount + 2)

    let repair = try #require(server.requests.first { $0.purpose == "summary-map-repair" }?.chat)
    #expect(repair.responseFormat?.jsonSchema?.name == "chunk_notes")
    #expect(repair.maxTokens == 1_500)
    #expect(repair.messages[0].content.contains("\"chunkIndex\": integer"))
    #expect(repair.messages[1].content.contains("Here are my notes, no JSON today."))
    #expect(repair.messages[1].content.contains("Validation error: "))

    let reduceUser = try #require(server.requests.last?.chat?.messages[1].content)
    for index in 0..<mapCount {
      #expect(reduceUser.contains("\"chunkIndex\" : \(index)"), "chunk \(index)")
    }
    #expect(!reduceUser.contains("\"chunkIndex\" : \(mapCount)"))
    #expect(
      output.usage
        == LLMUsage(
          promptTokens: 500 * mapCount + 700 + 2_000,
          completionTokens: 50 * mapCount + 60 + 400,
          requests: mapCount + 2))
  }

  @Test func aTruncatedMapAnswerFailsTheWholePassWithoutRepair() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    server.respond { request in
      request.purpose == "summary-map"
        ? Scripts.truncated("{\"chunkIndex\": 0, \"topics\": [")
        : Scripts.completion("{}")
    }
    let error = await #expect(throws: LLMError.self) {
      try await MapReduceTests.summarizer(server, contextTokens: 8_000).summarize(Self.input)
    }
    #expect(error == .truncated)
    #expect(server.requests.allSatisfy { $0.purpose == "summary-map" })
    #expect(server.requests.count <= 2, "the first truncated chunk cancels the rest")
  }

  @Test func theMapPathIsChosenExactlyWhenTheTranscriptExceedsTheInputBudget() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    server.respond(with: try MapReduceTests.responder())
    let tokens = TranscriptChunker.estimateTokens(Self.call.segments, language: "de")
    let overhead =
      TokenBudget.estimateTokens(
        SummaryPromptBuilder(template: Self.input.template).buildSingleShot(Self.input).messages[
          0
        ].content, language: "en") + 64
    var switched: Int?
    var seen = 0
    for contextTokens in stride(from: 12_000, through: 40_000, by: 4_000) {
      let summarizer = MapReduceTests.summarizer(server, contextTokens: contextTokens)
      _ = try await summarizer.summarize(Self.input)
      let purposes = server.requests.dropFirst(seen).map(\.purpose)
      seen = server.requests.count
      let single = purposes == ["summary"]
      let budget = TokenBudget(
        contextTokens: contextTokens, reservedOutputTokens: summarizer.reservedOutputTokens,
        promptOverheadTokens: overhead)
      #expect(single == budget.fits(tokens), "\(contextTokens): \(purposes)")
      if !single {
        #expect(purposes.last == "summary-reduce", "\(contextTokens)")
        #expect(purposes.dropLast().allSatisfy { $0 == "summary-map" }, "\(contextTokens)")
      }
      if single, switched == nil { switched = contextTokens }
    }
    let threshold = try #require(switched)
    #expect(threshold > 12_000 && threshold <= 32_000, "\(threshold)")
  }
}
