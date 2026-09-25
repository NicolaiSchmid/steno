import Foundation
import StenoCore

/// LLM: OpenAI-compatible base URL, model, context tokens, and the API key
/// in the keychain through `SecretStore` (never in `Settings`). Saving
/// rebuilds the pipeline; Test probes the endpoint through `LLMWiring`.
@MainActor
@Observable
final class LLMSettingsViewModel {
  enum TestResult: Equatable, Sendable {
    case success(String)
    case failure(String)
  }

  var baseURLText = ""
  var model = ""
  var contextTokensText = "32000"
  var apiKey = ""
  private(set) var savedKeyPresent = false
  private(set) var error: String?
  private(set) var testResult: TestResult?
  private(set) var isTesting = false
  private(set) var isConfigured = false
  private let environment: AppEnvironment

  init(environment: AppEnvironment) {
    self.environment = environment
  }

  func load() async {
    do {
      let settings = try await environment.settings.load()
      baseURLText = settings.llmBaseURL?.absoluteString ?? ""
      model = settings.llmModel ?? ""
      contextTokensText = String(settings.llmContextTokens)
      isConfigured = LLMWiring.isConfigured(settings)
      let key = try await environment.secrets.secret(for: .llmAPIKey)
      apiKey = key ?? ""
      savedKeyPresent = !(key ?? "").isEmpty
    } catch {
      self.error = "Settings could not be loaded: \(error)"
    }
  }

  /// The URL as typed, validated: http or https with a host.
  var baseURL: URL? {
    let trimmed = baseURLText.trimmingCharacters(in: .whitespaces)
    guard !trimmed.isEmpty, let url = URL(string: trimmed), let scheme = url.scheme?.lowercased(),
      scheme == "http" || scheme == "https", url.host() != nil
    else { return nil }
    return url
  }

  var contextTokens: Int? {
    Int(contextTokensText.trimmingCharacters(in: .whitespaces))
  }

  var validationMessage: String? {
    if !baseURLText.trimmingCharacters(in: .whitespaces).isEmpty, baseURL == nil {
      return "The base URL must start with http:// or https:// and name a host."
    }
    if let tokens = contextTokens, tokens < 1_024 {
      return "The context window must be at least 1024 tokens."
    }
    if contextTokens == nil, !contextTokensText.isEmpty {
      return "The context window must be a number."
    }
    return nil
  }

  func save() async {
    guard validationMessage == nil else {
      error = validationMessage
      return
    }
    do {
      var settings = try await environment.settings.load()
      settings.llmBaseURL = baseURL
      let trimmedModel = model.trimmingCharacters(in: .whitespaces)
      settings.llmModel = trimmedModel.isEmpty ? nil : trimmedModel
      settings.llmContextTokens = contextTokens ?? 32_000
      try await environment.settings.save(settings)
      let trimmedKey = apiKey.trimmingCharacters(in: .whitespaces)
      try await environment.secrets.setSecret(trimmedKey.isEmpty ? nil : trimmedKey, for: .llmAPIKey)
      savedKeyPresent = !trimmedKey.isEmpty
      isConfigured = LLMWiring.isConfigured(settings)
      try await environment.reloadPipeline()
      error = nil
    } catch {
      self.error = "Settings could not be saved: \(error)"
    }
  }

  /// Reachability, model listing and structured output mode, through the
  /// module's probe once StenoLLM is wired (`LLMWiring.probe`).
  func test() async {
    guard let baseURL else {
      testResult = .failure("Enter a valid base URL first.")
      return
    }
    let trimmedModel = model.trimmingCharacters(in: .whitespaces)
    guard !trimmedModel.isEmpty else {
      testResult = .failure("Enter the model name first.")
      return
    }
    isTesting = true
    defer { isTesting = false }
    var settings = Settings()
    settings.llmBaseURL = baseURL
    settings.llmModel = trimmedModel
    settings.llmContextTokens = contextTokens ?? 32_000
    let key = apiKey.trimmingCharacters(in: .whitespaces)
    do {
      let report = try await LLMWiring.probe(settings: settings, apiKey: key.isEmpty ? nil : key)
      testResult = .success(report)
    } catch {
      testResult = .failure(String(describing: error))
    }
  }
}
