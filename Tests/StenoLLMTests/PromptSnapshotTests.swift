import Foundation
import StenoCore
import Testing

@testable import StenoLLM

/// Golden prompts for every builder. Fixed meeting date, UTC, `en_US`
/// language names, so the files are identical on every machine. A diff
/// here is a reviewed prompt change and needs a sentence in the PR.
@Suite struct PromptSnapshotTests {
  static let standup = LLMFixtures.denglishStandup()

  /// One readable file per request: parameters, then each message.
  static func render(_ request: LLMRequest) -> String {
    var lines = [
      "purpose: \(request.purpose)",
      "temperature: \(request.temperature.map { String($0) } ?? "default")",
      "maxTokens: \(request.maxTokens.map(String.init) ?? "default")",
      "responseFormat: \(describe(request.responseFormat))",
    ]
    for message in request.messages {
      lines.append("")
      lines.append("=== \(message.role.rawValue) ===")
      lines.append(message.content)
    }
    return lines.joined(separator: "\n") + "\n"
  }

  static func describe(_ format: LLMResponseFormat) -> String {
    switch format {
    case .text: "text"
    case .jsonObject: "json_object"
    case .jsonSchema(let name, _, let strict): "json_schema \(name) strict=\(strict)"
    }
  }

  static func cleanupRequest(language: LanguageTag?) -> LLMRequest {
    let input = CleanupInput(export: standup)
    let chunks = TranscriptChunker(targetTokens: 150, maxTokens: 220).chunk(
      input.segments, language: language)
    return CleanupPromptBuilder().build(
      chunk: chunks[1], language: language, glossary: Glossary(input: input),
      labels: SpeakerLabels(speakers: input.speakers))
  }

  @Test func cleanupPromptGerman() throws {
    let request = Self.cleanupRequest(language: "de")
    try Snapshot.assert(Self.render(request), matches: "llm/prompts/cleanup-de.txt")
    #expect(request.messages[1].content.contains("Context (read-only, do not return):"))
    #expect(request.messages[1].content.contains("[context] Speaker"))
    #expect(request.messages[1].content.contains("[0] Speaker"))
  }

  @Test func cleanupPromptEnglish() throws {
    let request = Self.cleanupRequest(language: "en")
    try Snapshot.assert(Self.render(request), matches: "llm/prompts/cleanup-en.txt")
    #expect(request.messages[0].content.contains("Meeting language: English."))
  }
}
