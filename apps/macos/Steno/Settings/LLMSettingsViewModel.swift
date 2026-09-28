import Foundation
import StenoCore
import StenoLLM

/// The services the Summaries section offers by name. A preset is a base
/// URL plus what the form needs to show for it; the stored settings stay
/// URL, model and context size.
enum LLMPreset: String, CaseIterable, Identifiable, Sendable {
  case lmStudio
  case ollama
  case openRouter
  case openAI
  case anthropic
  case custom

  var id: String { rawValue }

  var title: String {
    switch self {
    case .lmStudio: "LM Studio on this Mac"
    case .ollama: "Ollama on this Mac"
    case .openRouter: "OpenRouter"
    case .openAI: "OpenAI"
    case .anthropic: "Anthropic"
    case .custom: "Custom server"
    }
  }

  /// nil for Custom: the user types the address.
  var baseURL: URL? {
    switch self {
    case .lmStudio: URL(string: "http://127.0.0.1:1234/v1")
    case .ollama: URL(string: "http://127.0.0.1:11434/v1")
    case .openRouter: URL(string: "https://openrouter.ai/api/v1")
    case .openAI: URL(string: "https://api.openai.com/v1")
    case .anthropic: URL(string: "https://api.anthropic.com/v1")
    case .custom: nil
    }
  }

  /// Local servers run without a key; the hosted ones need one.
  var needsAPIKey: Bool {
    switch self {
    case .lmStudio, .ollama, .custom: false
    case .openRouter, .openAI, .anthropic: true
    }
  }

  /// Hosted services have one address; local servers may sit on another
  /// port, so their field stays visible.
  var showsServerField: Bool {
    switch self {
    case .lmStudio, .ollama, .custom: true
    case .openRouter, .openAI, .anthropic: false
    }
  }

  var modelPlaceholder: String {
    switch self {
    case .lmStudio: "the model loaded in LM Studio"
    case .ollama: "llama3.1"
    case .openRouter: "openai/gpt-4.1-mini"
    case .openAI: "gpt-4.1-mini"
    case .anthropic: "claude-sonnet-5"
    case .custom: "model name"
    }
  }

  /// The preset whose address matches; Custom when none does, LM Studio
  /// when nothing is stored yet.
  static func infer(from url: URL?) -> LLMPreset {
    guard let url else { return .lmStudio }
    let normalized = Self.normalize(url)
    for preset in allCases where preset != .custom {
      if let candidate = preset.baseURL, Self.normalize(candidate) == normalized {
        return preset
      }
    }
    return .custom
  }

  private static func normalize(_ url: URL) -> String {
    var text = url.absoluteString.lowercased()
    while text.hasSuffix("/") { text.removeLast() }
    return text
  }
}

/// Summaries: the preset, the OpenAI-compatible base URL, model, context
/// tokens, and the API key in the keychain through `SecretStore` (never in
/// `Settings`). `commit()` saves when the draft differs from what is stored
/// and then probes the endpoint; `save()` and `test()` stay for direct use.
@MainActor
@Observable
final class LLMSettingsViewModel: SettingsSectionModel {
  enum TestResult: Equatable, Sendable {
    case success(String)
    case failure(String)
  }

  /// What the status row says.
  enum Status: Equatable, Sendable {
    case notConfigured
    case unchecked
    case checking
    case connected(String)
    case failed(String)
  }

  static let defaultContextTokens = 32_000

  /// The stored values, for `commit()` to compare against.
  private struct Stored: Equatable {
    var baseURL: URL?
    var model: String?
    var contextTokens: Int
    var apiKey: String?
  }

  var baseURLText = ""
  var model = ""
  var contextTokensText = String(LLMSettingsViewModel.defaultContextTokens)
  var apiKey = ""
  private(set) var preset: LLMPreset = .lmStudio
  var error: String?
  var errorDetails: String?
  private(set) var testResult: TestResult?
  private(set) var isTesting = false
  private(set) var isConfigured = false
  private var stored: Stored?
  private let environment: AppEnvironment

  init(environment: AppEnvironment) {
    self.environment = environment
  }

  func load() async {
    do {
      let settings = try await environment.settings.load()
      let key = try await environment.secrets.secret(for: .llmAPIKey)
      preset = LLMPreset.infer(from: settings.llmBaseURL)
      baseURLText = (settings.llmBaseURL ?? preset.baseURL)?.absoluteString ?? ""
      model = settings.llmModel ?? ""
      contextTokensText = String(settings.llmContextTokens)
      apiKey = key ?? ""
      isConfigured = LLMEndpoint(settings: settings) != nil
      stored = Stored(
        baseURL: settings.llmBaseURL, model: settings.llmModel,
        contextTokens: settings.llmContextTokens, apiKey: key)
    } catch {
      fail("Settings could not be loaded.", error)
    }
  }

  // MARK: Draft

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
      return "The server address must start with http:// or https:// and name a host."
    }
    if let tokens = contextTokens, tokens < 1_024 {
      return "The context size must be at least 1024."
    }
    if contextTokens == nil, !contextTokensText.isEmpty {
      return "The context size must be a number."
    }
    return nil
  }

  var status: Status {
    if isTesting { return .checking }
    switch testResult {
    case .success(let text): return .connected(text)
    case .failure(let text): return .failed(text)
    case nil: return isConfigured ? .unchecked : .notConfigured
    }
  }

  private var draft: Stored {
    let trimmedModel = model.trimmingCharacters(in: .whitespaces)
    let trimmedKey = apiKey.trimmingCharacters(in: .whitespaces)
    return Stored(
      baseURL: baseURL, model: trimmedModel.isEmpty ? nil : trimmedModel,
      contextTokens: contextTokens ?? Self.defaultContextTokens,
      apiKey: trimmedKey.isEmpty ? nil : trimmedKey)
  }

  // MARK: Actions

  /// Fills the address for the preset (Custom keeps what is typed) and
  /// stores it right away, so a later model entry completes the setup.
  func selectPreset(_ preset: LLMPreset) async {
    self.preset = preset
    if let url = preset.baseURL {
      baseURLText = url.absoluteString
    }
    testResult = nil
    await commit()
  }

  /// Saves when the form differs from what is stored and validates, then
  /// probes the endpoint when it is configured. Invalid input stays on
  /// screen as `validationMessage` and saves nothing.
  func commit() async {
    guard validationMessage == nil else { return }
    guard draft != stored else { return }
    await save()
    guard error == nil, isConfigured else { return }
    await test()
  }

  func save() async {
    guard validationMessage == nil else {
      error = validationMessage
      errorDetails = nil
      return
    }
    let draft = self.draft
    do {
      let settings = try await environment.updateSettings {
        $0.llmBaseURL = draft.baseURL
        $0.llmModel = draft.model
        $0.llmContextTokens = draft.contextTokens
      }
      try await environment.secrets.setSecret(draft.apiKey, for: .llmAPIKey)
      isConfigured = LLMEndpoint(settings: settings) != nil
      stored = draft
      try await environment.reloadPipeline()
      clearError()
    } catch {
      fail("Settings could not be saved.", error)
    }
  }

  /// Reachability, model listing and structured output mode, through the
  /// module's probe (`LLMWiring.probe`).
  func test() async {
    let draft = self.draft
    guard draft.baseURL != nil else {
      testResult = .failure("Enter a valid server address first.")
      return
    }
    guard draft.model != nil else {
      testResult = .failure("Enter the model name first.")
      return
    }
    isTesting = true
    defer { isTesting = false }
    var settings = Settings()
    settings.llmBaseURL = draft.baseURL
    settings.llmModel = draft.model
    settings.llmContextTokens = draft.contextTokens
    do {
      let report = try await LLMWiring.probe(settings: settings, apiKey: draft.apiKey)
      testResult = .success(report)
    } catch {
      testResult = .failure(String(describing: error))
    }
  }
}
