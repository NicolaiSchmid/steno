import Foundation
import StenoCore
import StenoLLM

/// The services the Summaries section offers by name. A preset is a base
/// URL plus what the form needs to show for it; the stored settings stay
/// URL, model and context size. `codex` is the one preset that is not an
/// address: it switches `Settings.llmProvider` and shows the consent card.
enum LLMPreset: String, CaseIterable, Identifiable, Sendable {
  case lmStudio
  case ollama
  case codex
  case openRouter
  case openAI
  case anthropic
  case custom

  var id: String { rawValue }

  var title: String {
    switch self {
    case .lmStudio: "LM Studio on this Mac"
    case .ollama: "Ollama on this Mac"
    case .codex: "ChatGPT (Codex)"
    case .openRouter: "OpenRouter"
    case .openAI: "OpenAI"
    case .anthropic: "Anthropic"
    case .custom: "Custom server"
    }
  }

  /// nil for Custom (the user types the address) and for ChatGPT (no
  /// address of its own).
  var baseURL: URL? {
    switch self {
    case .lmStudio: URL(string: "http://127.0.0.1:1234/v1")
    case .ollama: URL(string: "http://127.0.0.1:11434/v1")
    case .openRouter: URL(string: "https://openrouter.ai/api/v1")
    case .openAI: URL(string: "https://api.openai.com/v1")
    case .anthropic: URL(string: "https://api.anthropic.com/v1")
    case .codex, .custom: nil
    }
  }

  /// The provider the preset stands for.
  var provider: LLMProvider {
    self == .codex ? .codex : .endpoint
  }

  /// Local servers run without a key; the hosted ones need one. ChatGPT
  /// uses the Codex sign-in instead.
  var needsAPIKey: Bool {
    switch self {
    case .lmStudio, .ollama, .codex, .custom: false
    case .openRouter, .openAI, .anthropic: true
    }
  }

  /// Hosted services have one address; local servers may sit on another
  /// port, so their field stays visible.
  var showsServerField: Bool {
    switch self {
    case .lmStudio, .ollama, .custom: true
    case .codex, .openRouter, .openAI, .anthropic: false
    }
  }

  var modelPlaceholder: String {
    switch self {
    case .lmStudio: "the model loaded in LM Studio"
    case .ollama: "llama3.1"
    case .codex: "pick a model"
    case .openRouter: "openai/gpt-4.1-mini"
    case .openAI: "gpt-4.1-mini"
    case .anthropic: "claude-sonnet-5"
    case .custom: "model name"
    }
  }

  /// ChatGPT when that provider is stored, else the preset whose address
  /// matches.
  static func infer(from settings: Settings) -> LLMPreset {
    settings.llmProvider == .codex ? .codex : infer(from: settings.llmBaseURL)
  }

  /// The preset whose address matches; Custom when none does, LM Studio
  /// when nothing is stored yet.
  static func infer(from url: URL?) -> LLMPreset {
    guard let url else { return .lmStudio }
    let normalized = Self.normalize(url)
    for preset in allCases {
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
/// `Settings`); or, for the ChatGPT preset, the confirmation that Steno may
/// use the Codex sign-in on this Mac and the model picked from the
/// backend's list. `commit()` saves when the draft differs from what is
/// stored and then probes the endpoint; `save()` and `test()` stay for
/// direct use. Before `confirmCodex()` has stored `codexConfirmedAt`, the
/// only thing that touches the Codex sign-in is the consent card's account
/// line, read from the file while the ChatGPT choice is on screen.
@MainActor
@Observable
final class LLMSettingsViewModel: SettingsSectionModel {
  /// What the ChatGPT card knows about the sign-in on this Mac.
  enum CodexStatus: Equatable, Sendable {
    /// Not looked yet (the preset is not ChatGPT, or `load()` has not run).
    case notChecked
    /// `auth.json` with a ChatGPT login: the account line to show.
    case signedIn(String)
    /// No file or no ChatGPT tokens: the `CodexCredentialError` text.
    case unavailable(String)
  }

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
    var provider: LLMProvider = .endpoint
    var baseURL: URL?
    var model: String?
    var contextTokens: Int
    var apiKey: String?
    var codexModel: String?
    var codexContextTokens: Int = Settings.defaultCodexContextTokens
    var codexConfirmed = false
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
  /// ChatGPT: whether `codexConfirmedAt` is stored.
  private(set) var codexConfirmed = false
  private(set) var codexStatus: CodexStatus = .notChecked
  /// The backend's listed models, once fetched after confirmation.
  private(set) var codexModels: [CodexModel] = []
  /// Why the last model list failed (offline, plan limit); nil when it
  /// arrived. Separate from `codexStatus`, which is about the file.
  private(set) var codexModelsError: String?
  private(set) var isLoadingCodexModels = false
  /// The picked Codex model slug; empty until the list arrives or the user
  /// picks.
  private(set) var codexModel = ""
  private var codexContextTokens = Settings.defaultCodexContextTokens
  private var stored: Stored?
  /// Whether a key is in the keychain, as last loaded or saved; the draft
  /// `apiKey` may differ until `save()`.
  var hasStoredAPIKey: Bool { !(stored?.apiKey ?? "").isEmpty }
  /// Commits queue behind one another: focus loss and disappearing fire
  /// together, and two overlapping saves would rebuild the pipeline twice.
  private var commitTask: Task<Void, Never>?
  private let environment: AppEnvironment

  init(environment: AppEnvironment) {
    self.environment = environment
  }

  func load() async {
    do {
      let settings = try await environment.settings.load()
      let key = try await environment.secrets.secret(for: .llmAPIKey)
      preset = LLMPreset.infer(from: settings)
      let endpointPreset = LLMPreset.infer(from: settings.llmBaseURL)
      baseURLText = (settings.llmBaseURL ?? endpointPreset.baseURL)?.absoluteString ?? ""
      model = settings.llmModel ?? ""
      contextTokensText = String(settings.llmContextTokens)
      apiKey = key ?? ""
      codexModel = settings.codexModel ?? ""
      codexContextTokens = settings.codexContextTokens
      codexConfirmed = settings.codexConfirmedAt != nil
      isConfigured = LLMEndpoint(settings: settings) != nil
      stored = Stored(
        provider: settings.llmProvider, baseURL: settings.llmBaseURL, model: settings.llmModel,
        contextTokens: settings.llmContextTokens, apiKey: key, codexModel: settings.codexModel,
        codexContextTokens: settings.codexContextTokens, codexConfirmed: codexConfirmed)
      // A fresh install shows the local preset's address without owning it:
      // opening and leaving the section must not write settings or rebuild
      // the pipeline. The first real edit saves the address along.
      if settings.llmBaseURL == nil, settings.llmProvider == .endpoint { stored = draft }
      // The account line for the card or the status row; a file read, no
      // network, and only while the ChatGPT choice is on screen.
      if preset == .codex { await refreshCodexStatus() }
    } catch {
      fail("Settings could not be loaded.", error)
    }
  }

  // MARK: ChatGPT (Codex)

  /// Whether the sign-in is there, for the consent card's account line and
  /// the status row. Reads the file, never the network; after `load()` this
  /// is the one read that happens before confirmation, and only while the
  /// ChatGPT preset is on screen.
  func refreshCodexStatus() async {
    do {
      let credentials = try await environment.codexCredentials.stored()
      codexStatus = .signedIn(credentials.accountLine)
    } catch {
      codexStatus = .unavailable(String(describing: error))
    }
  }

  /// The consent card's primary button. The confirmation is stored before
  /// the tokens are used for anything: first the provider and
  /// `codexConfirmedAt` are saved (no model yet, so the endpoint stays off
  /// and nothing is probed), then the model list is fetched, the first
  /// listed model picked, and the result saved and probed.
  func confirmCodex() async {
    codexConfirmed = true
    testResult = nil
    await commit()
    guard error == nil else { return }
    await refreshCodexModels()
    if codexModel.isEmpty, let first = codexModels.first {
      codexModel = first.slug
      codexContextTokens = first.contextWindow ?? Settings.defaultCodexContextTokens
    }
    await commit()
  }

  /// "Stop using ChatGPT": clears the confirmation and returns to the
  /// endpoint provider with whatever it had.
  func stopUsingCodex() async {
    codexConfirmed = false
    codexModels = []
    codexModelsError = nil
    codexStatus = .notChecked
    preset = LLMPreset.infer(from: baseURL)
    testResult = nil
    await commit()
  }

  /// The backend's listed models. Only after confirmation: the list needs
  /// the sign-in.
  func refreshCodexModels() async {
    guard codexConfirmed else { return }
    isLoadingCodexModels = true
    defer { isLoadingCodexModels = false }
    await refreshCodexStatus()
    do {
      codexModels = try await LLMWiring.codexModels(codexCredentials: environment.codexCredentials)
      codexModelsError = nil
      if let current = codexModels.first(where: { $0.slug == codexModel }),
        let window = current.contextWindow
      {
        codexContextTokens = window
      }
    } catch let error as CodexCredentialError {
      // The sign-in itself is the problem; the card says so.
      codexStatus = .unavailable(String(describing: error))
      codexModelsError = nil
    } catch {
      codexModelsError = "The model list could not be loaded. \(error)"
    }
  }

  /// The picker's choice: the slug and its context window, then save.
  func selectCodexModel(_ slug: String) async {
    codexModel = slug
    if let picked = codexModels.first(where: { $0.slug == slug }), let window = picked.contextWindow
    {
      codexContextTokens = window
    }
    testResult = nil
    await commit()
  }

  // MARK: Draft

  /// The picker's rows: the listed models plus the stored slug when the
  /// list does not carry it (a retired model keeps working until changed).
  var codexModelChoices: [CodexModel] {
    var choices = codexModels
    if !codexModel.isEmpty, !choices.contains(where: { $0.slug == codexModel }) {
      choices.append(CodexModel(slug: codexModel, displayName: codexModel))
    }
    return choices
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
    if preset == .codex { return nil }
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
      provider: preset.provider,
      // Under ChatGPT the server field is off screen; whatever it held stays.
      baseURL: preset == .codex ? (baseURL ?? stored?.baseURL) : baseURL,
      model: trimmedModel.isEmpty ? nil : trimmedModel,
      contextTokens: contextTokens ?? Self.defaultContextTokens,
      apiKey: trimmedKey.isEmpty ? nil : trimmedKey,
      codexModel: codexModel.isEmpty ? nil : codexModel,
      codexContextTokens: codexContextTokens,
      codexConfirmed: codexConfirmed)
  }

  /// A `Settings` with this draft's LLM fields, for the probe.
  private func settings(from draft: Stored) -> Settings {
    var settings = Settings()
    settings.llmProvider = draft.provider
    settings.llmBaseURL = draft.baseURL
    settings.llmModel = draft.model
    settings.llmContextTokens = draft.contextTokens
    settings.codexModel = draft.codexModel
    settings.codexContextTokens = draft.codexContextTokens
    settings.codexConfirmedAt = draft.codexConfirmed ? Date() : nil
    return settings
  }

  // MARK: Actions

  /// Fills the address for the preset (Custom keeps what is typed) and
  /// stores it right away, so a later model entry completes the setup.
  /// ChatGPT stores nothing until the consent card's button: showing the
  /// card reads the sign-in file for the account line, nothing more.
  func selectPreset(_ preset: LLMPreset) async {
    self.preset = preset
    if let url = preset.baseURL {
      baseURLText = url.absoluteString
    }
    testResult = nil
    if preset == .codex {
      await refreshCodexStatus()
      if codexConfirmed { await commit() }
      return
    }
    await commit()
  }

  /// Saves when the form differs from what is stored and validates, then
  /// probes the endpoint when it is configured. Invalid input stays on
  /// screen as `validationMessage` and saves nothing.
  func commit() async {
    let previous = commitTask
    let task = Task { [weak self] in
      await previous?.value
      await self?.performCommit()
    }
    commitTask = task
    await task.value
  }

  private func performCommit() async {
    guard validationMessage == nil else { return }
    // The ChatGPT choice stores nothing until its button: the pane closing
    // with the card on screen must not switch the provider.
    guard !(preset == .codex && !codexConfirmed) else { return }
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
        $0.llmProvider = draft.provider
        $0.llmBaseURL = draft.baseURL
        $0.llmModel = draft.model
        $0.llmContextTokens = draft.contextTokens
        $0.codexModel = draft.codexModel
        $0.codexContextTokens = draft.codexContextTokens
        if draft.codexConfirmed {
          if $0.codexConfirmedAt == nil { $0.codexConfirmedAt = Date() }
        } else {
          $0.codexConfirmedAt = nil
        }
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
    if draft.provider == .codex {
      guard draft.codexConfirmed else {
        testResult = .failure("Confirm the use of your ChatGPT account first.")
        return
      }
      guard draft.codexModel != nil else {
        testResult = .failure("Pick a model first.")
        return
      }
    } else {
      guard draft.baseURL != nil else {
        testResult = .failure("Enter a valid server address first.")
        return
      }
      guard draft.model != nil else {
        testResult = .failure("Enter the model name first.")
        return
      }
    }
    isTesting = true
    defer { isTesting = false }
    let probed = settings(from: draft)
    do {
      let report = try await LLMWiring.probe(
        settings: probed, apiKey: draft.apiKey, codexCredentials: environment.codexCredentials)
      testResult = .success(report)
    } catch {
      testResult = .failure(String(describing: error))
    }
  }
}
