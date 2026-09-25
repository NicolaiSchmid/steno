import Foundation
import StenoCore
import Testing

@testable import StenoLLM

/// One real endpoint, opt-in: `STENO_LLM_TESTS=1` with `STENO_LLM_BASE_URL`,
/// `STENO_LLM_MODEL` and optionally `STENO_LLM_API_KEY`. Runs the Denglish
/// fixture through cleanup and the Default template and asserts the
/// contract, not the wording. Skipped with a message naming the switch.
@Suite(
  .serialized,
  .enabled(
    if: ProcessInfo.processInfo.environment["STENO_LLM_TESTS"] == "1",
    "Set STENO_LLM_TESTS=1 with STENO_LLM_BASE_URL and STENO_LLM_MODEL to run against a real endpoint."
  ))
struct LiveEndpointTests {
  static func endpoint() throws -> LLMEndpoint {
    let environment = ProcessInfo.processInfo.environment
    guard let base = environment["STENO_LLM_BASE_URL"], let url = URL(string: base),
      let model = environment["STENO_LLM_MODEL"], !model.isEmpty
    else {
      throw LLMError.notConfigured("STENO_LLM_BASE_URL and STENO_LLM_MODEL")
    }
    var endpoint = LLMEndpoint(baseURL: url, model: model)
    if let context = environment["STENO_LLM_CONTEXT_TOKENS"].flatMap({ Int($0) }) {
      endpoint.contextTokens = context
    }
    return endpoint
  }

  static func client() throws -> OpenAICompatibleClient {
    OpenAICompatibleClient(
      endpoint: try endpoint(), apiKey: ProcessInfo.processInfo.environment["STENO_LLM_API_KEY"],
      observer: { event in FileHandle.standardError.write(Data("live: \(event)\n".utf8)) })
  }

  @Test func probeReachesTheEndpoint() async throws {
    let probe = try await Self.client().probe()
    #expect(probe.reachable)
    print("live probe: \(probe)")
  }

  @Test func cleanupPreservesTheSegmentCountAndFixesSomething() async throws {
    let client = try Self.client()
    let input = CleanupInput(export: LLMFixtures.denglishStandup())
    let output = try await LLMTranscriptCleaner(model: client, endpoint: client.endpoint).clean(
      input)
    #expect(output.segments.count == input.segments.count)
    #expect(output.segments.map(\.id) == input.segments.map(\.id))
    #expect(output.segments.map(\.rawText) == input.segments.map(\.rawText))
    #expect(output.failedChunks.isEmpty, "failed chunks: \(output.failedChunks)")
    #expect(output.segments.map(\.text) != input.segments.map(\.text), "nothing was corrected")
    #expect(output.usage.requests >= 1)
    print("live cleanup: \(output.usage), mode \(await client.resolvedMode)")
  }

  @Test func summaryDecodesWithATitleAndABullet() async throws {
    let client = try Self.client()
    let input = SummaryInput(
      export: LLMFixtures.denglishStandup(), template: SummaryTemplate.bundled(id: "default"))
    let output = try await LLMMeetingSummarizer(model: client, endpoint: client.endpoint)
      .summarize(input)
    #expect(!output.title.isEmpty)
    #expect(output.summary.sections.first?.id == "executive-summary")
    #expect(output.summary.sections.first?.bullets.isEmpty == false)
    #expect(output.usage.requests >= 1)
    #expect(output.usage.promptTokens > 0)
    print("live summary: \(output.title) / \(output.usage), mode \(await client.resolvedMode)")
  }
}
