import Foundation
import StenoCore

/// Builds the pass 1 request for one chunk: fix speech-to-text mistakes,
/// keep count, order and wording. Temperature 0. Pinned by the goldens in
/// `Tests/Fixtures/llm/prompts/cleanup-*.txt`.
public struct CleanupPromptBuilder: Sendable {
  /// The endpoint's ceiling for one answer.
  public var maxOutputTokens: Int

  public init(maxOutputTokens: Int = 4_096) {
    self.maxOutputTokens = maxOutputTokens
  }

  public static let outputSchema = JSONSchema.object([
    "segments": .array(
      of: .object([
        "index": .integer(description: "the segment's number as given"),
        "text": .string(description: "the corrected text"),
      ]))
  ])

  /// `glossary` is `CleanupInput.glossary`: the names to spell exactly.
  public func build(
    chunk: TranscriptChunk, language: LanguageTag?, glossary: [String], labels: SpeakerLabels
  ) -> LLMRequest {
    let count = chunk.segments.count
    let languageName = OutputLanguage.promptName(OutputLanguage.resolve(meeting: language))
    var system = [
      "You are Steno's transcript editor. You receive numbered segments of a speech-to-text transcript and return the same segments, corrected, as one JSON object and nothing else.",
      "",
      "Meeting language: \(languageName). Speakers may mix \(languageName) and English; keep every code-switch exactly as spoken and never translate.",
    ]
    if !glossary.isEmpty {
      system.append("Names to spell exactly like this: \(glossary.joined(separator: ", ")).")
    }
    system += [
      "",
      "Rules:",
      "- Return exactly \(count) segments with the indices 0 to \(count - 1), each once, in order. Never merge, split, drop, add, shorten, expand or summarise a segment.",
      "- Fix speech-to-text mistakes only: misheard anglicisms and product names (\"git hub\" to \"GitHub\", \"kuber netes\" to \"Kubernetes\"), the names listed above, German noun capitalisation, sentence-initial capitals, punctuation.",
      "- Keep the wording, the word order and the speaker's register. Keep fillers unless they are transcription noise.",
      "- When a segment needs no change, return its text unchanged.",
      "- Lines marked as context are read-only and are not part of the answer.",
      "",
      "Return exactly this JSON shape:",
      Self.outputSchema.promptText,
    ]
    var user: [String] = []
    if !chunk.leadingContext.isEmpty {
      user.append("Context (read-only, do not return):")
      user.append(
        chunk.leadingContext.map { "[context] \(labels.label(for: $0.speakerID)): \($0.text)" }
          .joined(separator: "\n"))
      user.append("")
    }
    user.append("Segments to correct (\(count)):")
    user.append(TranscriptLines.render(chunk.segments, labels: labels))
    return LLMRequest(
      messages: [
        LLMMessage(role: .system, content: system.joined(separator: "\n")),
        LLMMessage(role: .user, content: user.joined(separator: "\n")),
      ],
      responseFormat: .jsonSchema(
        name: "transcript_cleanup", schema: Self.outputSchema.jsonValue, strict: true),
      temperature: 0,
      maxTokens: outputTokens(for: chunk, language: language),
      purpose: "cleanup")
  }

  /// The same request with the rejected answer and the reason appended, for
  /// the one retry.
  public func buildRetry(_ request: LLMRequest, previousAnswer: String, error: String)
    -> LLMRequest
  {
    var retry = request
    retry.messages.append(LLMMessage(role: .assistant, content: previousAnswer))
    retry.messages.append(
      LLMMessage(
        role: .user,
        content:
          "That answer was rejected: \(error) Return the JSON again with every segment, the same indices and the wording kept."
      ))
    retry.purpose = "cleanup-retry"
    return retry
  }

  /// The answer repeats the chunk as JSON: its text plus about twelve
  /// tokens of framing per segment, with a third of headroom.
  func outputTokens(for chunk: TranscriptChunk, language: LanguageTag?) -> Int {
    let text = TokenBudget.estimateTokens(
      chunk.segments.map(\.text).joined(separator: "\n"), language: language)
    let estimate = (text + chunk.segments.count * 12 + 64) * 4 / 3
    return min(max(estimate, 256), maxOutputTokens)
  }
}

extension CleanupInput {
  /// Names the cleanup pass must spell exactly: participants (calendar
  /// attendees included) first, then known people, each once
  /// (case-insensitively), in order of appearance. No product glossary in v1.
  public var glossary: [String] {
    var seen: Set<String> = []
    return (participants.map(\.displayName) + knownPeople.map(\.displayName)).compactMap { name in
      let trimmed = name.trimmingCharacters(in: .whitespaces)
      guard !trimmed.isEmpty, seen.insert(trimmed.lowercased()).inserted else { return nil }
      return trimmed
    }
  }
}
