import Foundation
import StenoCore
import StenoLLM

/// The LLM passes for the pipeline, built from `Settings` and the API key:
/// `LLMEndpoint(settings:)` (nil until the chosen provider is set up; the
/// pipeline then skips the cleanup and summary passes), one client shared by
/// the cleaner and the summarizer so a structured-output mode learned during
/// cleanup carries over. The endpoint provider's key goes into the client's
/// init only; the Codex provider reads the Codex CLI's sign-in through
/// `CodexCredentialStore` and stores nothing of its own. Neither is ever
/// part of `Settings`.
enum LLMWiring {
  typealias Passes = (cleaner: any TranscriptCleaner, summarizer: any MeetingSummarizer)

  struct NotConfigured: Error, CustomStringConvertible {
    var description: String { "Set up a summaries service first." }
  }

  static func passes(settings: Settings, apiKey: String?, codexCredentials: CodexCredentialStore)
    -> Passes?
  {
    guard let endpoint = LLMEndpoint(settings: settings) else { return nil }
    let client = makeClient(
      endpoint: endpoint, apiKey: apiKey, codexCredentials: codexCredentials, retry: .default)
    return (
      LLMTranscriptCleaner(model: client, endpoint: endpoint),
      LLMMeetingSummarizer(model: client, endpoint: endpoint)
    )
  }

  static func makeClient(
    endpoint: LLMEndpoint, apiKey: String?, codexCredentials: CodexCredentialStore,
    retry: RetryPolicy
  ) -> any LLMClient {
    if endpoint.isCodexBackend {
      return CodexResponsesClient(endpoint: endpoint, credentials: codexCredentials, retry: retry)
    }
    return OpenAICompatibleClient(endpoint: endpoint, apiKey: apiKey, retry: retry)
  }

  /// One line for the Test button: whether `/models` lists the model, the
  /// structured output mode the server accepted and the round trip. Throws
  /// the client's `LLMError` (its description is the failure text) when the
  /// endpoint does not answer or rejects the key, or the
  /// `CodexCredentialError` when there is no usable Codex sign-in.
  static func probe(settings: Settings, apiKey: String?, codexCredentials: CodexCredentialStore)
    async throws
    -> String
  {
    guard let endpoint = LLMEndpoint(settings: settings) else { throw NotConfigured() }
    // One attempt: a button press should answer at once, not after the
    // pipeline's retry backoff.
    let client = makeClient(
      endpoint: endpoint, apiKey: apiKey, codexCredentials: codexCredentials, retry: .none)
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
    let who = report.accountLine.map { " as \($0)" } ?? ""
    return
      "Connected\(who): \(listed), structured output \(report.resolvedMode.rawValue), \(milliseconds) ms."
  }

  /// The Codex models on offer, listed ones only, for the Summaries picker.
  /// Requires a confirmed sign-in; the model list does not depend on the
  /// endpoint's model, so a placeholder stands in.
  static func codexModels(codexCredentials: CodexCredentialStore) async throws -> [CodexModel] {
    let client = CodexResponsesClient(
      endpoint: .codex(model: "list", contextTokens: Settings.defaultCodexContextTokens),
      credentials: codexCredentials, retry: .none)
    return try await client.listModels().filter(\.isListed)
  }
}
