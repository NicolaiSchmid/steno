import Foundation
import StenoCore

/// The LLM passes for the pipeline, built from `Settings` and the API key.
/// Returns nil (the pipeline runs `PassthroughCleaner` and `FakeSummarizer`)
/// until the StenoLLM workstream (PR #5) merges and this file wires
/// `LLMEndpoint(settings:)` and `OpenAICompatibleClient` in; the app shows
/// the unconfigured state in the LLM settings meanwhile.
enum LLMWiring {
  typealias Passes = (cleaner: any TranscriptCleaner, summarizer: any MeetingSummarizer)

  struct NotWired: Error, CustomStringConvertible {
    var description: String {
      "The LLM client is not part of this build yet (StenoLLM, PR #5)."
    }
  }

  static func passes(settings: Settings, apiKey: String?) -> Passes? {
    nil
  }

  /// One line describing what the endpoint answered, for the Test button.
  static func probe(settings: Settings, apiKey: String?) async throws -> String {
    throw NotWired()
  }

  /// Whether the settings name an endpoint the passes can use.
  static func isConfigured(_ settings: Settings) -> Bool {
    settings.llmBaseURL != nil && !(settings.llmModel ?? "").isEmpty
  }
}
