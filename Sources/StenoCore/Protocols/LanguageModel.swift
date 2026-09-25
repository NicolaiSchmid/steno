/// One completion against an OpenAI-compatible endpoint. The only code path
/// besides `Destination` that may send bytes off the device, and it sends
/// text only.
public protocol LanguageModel: Sendable {
  func complete(_ request: LLMRequest) async throws -> LLMResponse
}
