import Foundation
import StenoCore
import Testing

@testable import StenoLLM

@Suite struct CleanupTests {
  static let standup = LLMFixtures.denglishStandup()
  static let call = LLMFixtures.customerCall60min()

  /// A cleaner over the stub server with no retries and a real clock; every
  /// request is answered at once, so nothing ever sleeps.
  static func cleaner(
    _ server: StubChatServer, chunker: TranscriptChunker? = nil,
    configure: (inout LLMEndpoint) -> Void = { _ in }
  ) -> LLMTranscriptCleaner {
    var endpoint = LLMEndpoint(baseURL: server.baseURL, model: "stub-model")
    configure(&endpoint)
    let client = OpenAICompatibleClient(endpoint: endpoint, apiKey: nil, retry: .none)
    return LLMTranscriptCleaner(model: client, endpoint: endpoint, chunker: chunker)
  }

  /// A "perfect" model: capitalises the first letter and fixes two known
  /// STT errors, keeping the word count.
  static let fixing: @Sendable (Int, String) -> String? = { _, text in
    var fixed = text.replacingOccurrences(of: "git hub", with: "GitHub")
    fixed = fixed.replacingOccurrences(of: "jerome", with: "Jérôme")
    return fixed.prefix(1).uppercased() + fixed.dropFirst()
  }

  @Test func preservesCountOrderIDsAndRawTextAndRewritesText() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    server.respond(with: Scripts.cleanupEcho(transform: Self.fixing))
    let input = CleanupInput(export: Self.standup)
    let output = try await Self.cleaner(
      server, chunker: TranscriptChunker(targetTokens: 150, maxTokens: 220)
    ).clean(input)

    #expect(output.segments.count == input.segments.count)
    #expect(output.segments.map(\.id) == input.segments.map(\.id))
    #expect(output.segments.map(\.rawText) == input.segments.map(\.rawText))
    #expect(output.segments.map(\.speakerID) == input.segments.map(\.speakerID))
    #expect(output.segments.map(\.start) == input.segments.map(\.start))
    #expect(output.failedChunks == [])
    #expect(output.segments[2].text.contains("GitHub"))
    #expect(output.segments[4].text.contains("Jérôme"))
    #expect(output.segments.allSatisfy { $0.text.first?.isUppercase == true })
    let requests = server.requests
    #expect(requests.count >= 2, "the small chunker splits 24 segments into several chunks")
    #expect(requests.allSatisfy { $0.purpose == "cleanup" })
    #expect(
      output.usage
        == LLMUsage(
          promptTokens: 100 * requests.count, completionTokens: 50 * requests.count,
          requests: requests.count))
    let system = try #require(requests.first?.chat?.messages.first?.content)
    #expect(system.contains("Names to spell exactly like this: Mara, Jérôme, Nicolai."))
    #expect(system.contains("Meeting language: German."))
    #expect(requests.first?.chat?.temperature == 0)
    #expect(requests.first?.chat?.responseFormat?.jsonSchema?.name == "transcript_cleanup")
  }

  @Test func aWrongCountChunkIsRetriedOnceThenKeptRaw() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    let chunker = TranscriptChunker(targetTokens: 150, maxTokens: 220)
    let input = CleanupInput(export: Self.standup)
    let chunks = chunker.chunk(input.segments, language: input.language)
    try #require(chunks.count >= 3)
    let victimChunk = chunks[1]
    let victim = try #require(victimChunk.segments.first?.text)
    // Drops the last segment of the victim chunk's answer, both times.
    server.respond { request in
      guard let firstUser = request.chat?.messages.first(where: { $0.role == "user" })?.content
      else { return nil }
      let segments = Scripts.parseSegments(firstUser)
      let sabotage = segments.first?.text == victim
      let draft = CleanupDraft(
        segments: segments.dropLast(sabotage ? 1 : 0).map { .init(index: $0.index, text: $0.text) })
      return Scripts.json(draft)
    }
    let output = try await Self.cleaner(server, chunker: chunker).clean(input)

    #expect(output.failedChunks == [victimChunk.index])
    #expect(
      output.segments.map(\.text) == input.segments.map(\.text),
      "the echo changes nothing, the failed chunk stays raw")
    #expect(output.segments.count == input.segments.count)
    let purposes = server.requests.map(\.purpose)
    #expect(purposes.filter { $0 == "cleanup-retry" }.count == 1)
    #expect(server.requests.count == chunks.count + 1)
    let retry = try #require(server.requests.first { $0.purpose == "cleanup-retry" }?.chat)
    #expect(retry.messages.count == 4)
    #expect(retry.messages[2].role == "assistant")
    #expect(
      retry.messages[3].content.contains(
        "Expected \(victimChunk.segments.count) segments, got \(victimChunk.segments.count - 1)."))
    #expect(output.usage.requests == chunks.count + 1)
  }

  @Test func rewordedSegmentsAreRejectedButOneWordDifferencesPass() throws {
    let chunk = TranscriptChunker().chunk(Self.standup.segments, language: "de")[0]
    let validator = CleanupValidator()
    var draft = CleanupDraft(
      segments: chunk.segments.enumerated().map { .init(index: $0.offset, text: $0.element.text) })
    #expect(try validator.validate(draft, against: chunk) == chunk.segments.map(\.text))

    draft.segments[2].text =
      "Heute schaue ich mir die flaky Tests in der CI-Pipeline an, die laufen seit dem GitHub-Upgrade nicht mehr stabil."
    #expect(try validator.validate(draft, against: chunk)[2].hasPrefix("Heute"))

    draft.segments[3].text = "Blocker?"
    var rejected = #expect(throws: CleanupValidator.Rejection.self) {
      try validator.validate(draft, against: chunk)
    }
    #expect(rejected?.reasons.first?.hasPrefix("Segment 3 changed from 4 to 1 words") == true)

    draft.segments[3].text = "hast du einen blocker?"
    draft.segments[5].text = ""
    rejected = #expect(throws: CleanupValidator.Rejection.self) {
      try validator.validate(draft, against: chunk)
    }
    #expect(rejected?.reasons == ["Segment 5 came back empty."])

    draft.segments[5].text = chunk.segments[5].text
    draft.segments[0].index = 7
    rejected = #expect(throws: CleanupValidator.Rejection.self) {
      try validator.validate(draft, against: chunk)
    }
    #expect(rejected?.reasons.first?.hasPrefix("Indices must be 0 to") == true)
  }

  @Test func neverExceedsMaxConcurrentRequests() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    server.respond(with: Scripts.cleanupEcho())
    server.holdResponses()
    let input = CleanupInput(export: Self.call)
    let chunker = TranscriptChunker()
    let chunkCount = chunker.chunk(input.segments, language: input.language).count
    #expect(chunkCount >= 6)
    let cleaner = Self.cleaner(server, chunker: chunker) { $0.maxConcurrentRequests = 2 }
    let task = Task { try await cleaner.clean(input) }
    await server.received(atLeast: 2)
    for _ in 0..<200 { await Task.yield() }
    #expect(server.requests.count == 2, "the third chunk waits for a free slot")
    #expect(server.inFlight == 2)
    server.release()
    let output = try await task.value
    #expect(output.failedChunks == [])
    #expect(server.requests.count == chunkCount)
    #expect(server.maxInFlight == 2)
    #expect(output.usage.requests == chunkCount)
  }

  @Test func transportFailuresPropagateInsteadOfFallingBackToRaw() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    // Every chunk in flight gets the same answer, so the first error to
    // surface is always this one.
    server.respond { _ in Scripts.unauthorized() }
    let cleaner = Self.cleaner(
      server, chunker: TranscriptChunker(targetTokens: 150, maxTokens: 220))
    let error = await #expect(throws: LLMError.self) {
      try await cleaner.clean(CleanupInput(export: Self.standup))
    }
    #expect(error == .http(status: 401, body: "Incorrect API key provided"))
  }

  @Test func undecodableAndTruncatedAnswersFallBackToRawAfterOneRetry() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    server.enqueue(contentsOf: [
      Scripts.completion("not json at all"), Scripts.truncated("{\"segments\": ["),
    ])
    let chunker = TranscriptChunker()
    let cleaner = Self.cleaner(server, chunker: chunker)
    let output = try await cleaner.clean(CleanupInput(export: Self.standup))
    #expect(chunker.chunk(Self.standup.segments, language: "de").count == 1)
    #expect(output.failedChunks == [0])
    #expect(output.segments == Self.standup.segments)
    #expect(server.requests.count == 2)
    #expect(server.requests.last?.chat?.messages.last?.content.contains("invalid JSON") == true)
  }

  @Test func emptyTranscriptMakesNoRequest() async throws {
    let server = try StubChatServer()
    defer { server.stop() }
    var input = CleanupInput(export: Self.standup)
    input.segments = []
    let output = try await Self.cleaner(server).clean(input)
    #expect(output.segments.isEmpty)
    #expect(output.usage == .zero)
    #expect(server.requests.isEmpty)
  }

  @Test func glossaryDeduplicatesAndOrdersParticipantsFirst() {
    var input = CleanupInput(export: Self.standup)
    input.knownPeople.append(
      Person(id: SampleData.uuid(99), displayName: "nicolai", createdAt: SampleData.createdAt))
    input.knownPeople.append(
      Person(id: SampleData.uuid(98), displayName: "  ", createdAt: SampleData.createdAt))
    #expect(Glossary(input: input).people == ["Mara", "Jérôme", "Nicolai"])
    #expect(
      Glossary(input: CleanupInput(export: Self.call)).people == [
        "Nicolai", "Petra Vogel", "Tom Berger", "Jérôme",
      ])
  }

  @Test func outputTokensScaleWithTheChunkAndStayUnderTheCeiling() {
    let builder = CleanupPromptBuilder(maxOutputTokens: 1_000)
    let chunks = TranscriptChunker().chunk(Self.call.segments, language: "de")
    #expect(builder.outputTokens(for: chunks[0], language: "de") == 1_000)
    let small = TranscriptChunker(targetTokens: 50, maxTokens: 80).chunk(
      Self.standup.segments, language: "de")[0]
    let tokens = builder.outputTokens(for: small, language: "de")
    #expect(tokens >= 256 && tokens < 1_000, "\(tokens)")
  }
}
