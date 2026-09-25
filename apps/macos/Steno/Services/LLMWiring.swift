import Foundation
import StenoCore
import StenoLLM

/// The LLM passes for the pipeline, built from `Settings` and the API key:
/// `LLMEndpoint(settings:)` (nil until URL and model are set, then the
/// pipeline runs `PassthroughCleaner` and `FakeSummarizer` as the CLI does),
/// one `OpenAICompatibleClient` shared by the cleaner and the summarizer so
/// a structured-output mode learned during cleanup carries over. The key
/// goes into the client's init only; it is never part of `Settings`.
enum LLMWiring {
  typealias Passes = (cleaner: any TranscriptCleaner, summarizer: any MeetingSummarizer)

  struct NotConfigured: Error, CustomStringConvertible {
    var description: String { "Enter a base URL and a model name first." }
  }

  static func passes(settings: Settings, apiKey: String?) -> Passes? {
    guard let endpoint = LLMEndpoint(settings: settings) else { return nil }
    let client = OpenAICompatibleClient(endpoint: endpoint, apiKey: apiKey)
    return (
      LLMTranscriptCleaner(model: client, endpoint: endpoint),
      LLMMeetingSummarizer(model: client, endpoint: endpoint)
    )
  }

  /// One line for the Test button: whether `/models` lists the model, the
  /// structured output mode the server accepted and the round trip. Throws
  /// the client's `LLMError` (its description is the failure text) when the
  /// endpoint does not answer or rejects the key.
  static func probe(settings: Settings, apiKey: String?) async throws -> String {
    guard let endpoint = LLMEndpoint(settings: settings) else { throw NotConfigured() }
    // One attempt: a button press should answer at once, not after the
    // pipeline's retry backoff.
    let client = OpenAICompatibleClient(endpoint: endpoint, apiKey: apiKey, retry: .none)
    let report = try await client.probe()
    let listed: String
    switch report.modelListed {
    case .some(true): listed = "model listed"
    case .some(false): listed = "model not in /models"
    case .none: listed = "no model list"
    }
    let milliseconds =
      report.roundTrip.components.seconds * 1000
      + report.roundTrip.components.attoseconds / 1_000_000_000_000_000
    return
      "Connected: \(listed), structured output \(report.resolvedMode.rawValue), \(milliseconds) ms."
  }

  /// Whether the settings name an endpoint the passes can use.
  static func isConfigured(_ settings: Settings) -> Bool {
    LLMEndpoint(settings: settings) != nil
  }
}
