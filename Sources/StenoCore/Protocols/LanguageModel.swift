/// One completion against the configured model service (an
/// OpenAI-compatible endpoint or the Codex backend). The only code path
/// besides `Destination` that may send bytes off the device, and it sends
/// text only.
public protocol LanguageModel: Sendable {
  func complete(_ request: LLMRequest) async throws -> LLMResponse
}
