import Foundation
import StenoCore

/// Onboarding in two pages. Page 1, permissions: microphone, system audio
/// (both required), then calendar (optional) and local network (optional;
/// the prompt comes with the first phone pairing, so this step only
/// explains); under the intro, one sentence says what happens to the
/// recordings under the stored retention rule. Page 2, "Summaries and
/// export": the LLM endpoint and the Obsidian vault, both optional, written
/// through the same `LLMSettingsViewModel` and `ObsidianSettingsViewModel`
/// the Settings tabs use. The model owns the exit: Finish, or both rows
/// handled on page 2, set `steno.onboardingCompleted` and `finished`, which
/// the view turns into the window's dismissal; the view marks the flag on
/// the window's close button too. Installs without the flag open once more,
/// straight on page 2 when the permissions are already granted, and not at
/// all when Settings already holds an endpoint and a vault.
@MainActor
@Observable
final class OnboardingViewModel {
  struct Step: Identifiable, Equatable, Sendable {
    var kind: PermissionKind
    var state: PermissionState
    var id: PermissionKind { kind }
    var isRequired: Bool { kind.isRequired }
  }

  enum Page: Equatable, Sendable {
    case permissions
    case setup
  }

  /// The two optional rows of page 2.
  enum SetupStep: CaseIterable, Identifiable, Sendable {
    case summaries
    case vault

    var id: Self { self }

  }

  enum SetupState: Equatable, Sendable {
    case open
    /// Saved, with the collapsed row's line ("Saved: <model> at <host>" or
    /// "Saved: <model> via ChatGPT as <account>").
    case saved(String)
    case skipped

    var isHandled: Bool { self != .open }
  }

  static let onboardingCompletedKey = "steno.onboardingCompleted"

  private(set) var steps = PermissionKind.allCases.map { Step(kind: $0, state: .unknown) }
  private(set) var requesting: PermissionKind?
  private(set) var skipped: Set<PermissionKind> = []
  private(set) var page: Page = .permissions
  private(set) var setupStates: [SetupStep: SetupState] = [.summaries: .open, .vault: .open]
  /// Set by `finish()`: the flag is written and the window should close.
  private(set) var finished = false
  /// What happens to recordings under the stored rule and where to change
  /// it; nil until `load()` ran with a settings store.
  private(set) var retentionSentence: String?
  /// The page 2 rows' view models, the Settings tabs' own; nil under
  /// `init(permissions:)`, where the rows can only be skipped.
  let llm: LLMSettingsViewModel?
  let obsidian: ObsidianSettingsViewModel?
  private let permissions: any PermissionsChecking
  private let settings: SettingsStore?
  /// Where `steno.onboardingCompleted` is recorded.
  private let defaults: UserDefaults
  private var loaded = false

  init(
    permissions: any PermissionsChecking, settings: SettingsStore? = nil,
    environment: AppEnvironment? = nil, defaults: UserDefaults = .standard
  ) {
    self.permissions = permissions
    self.settings = settings
    self.defaults = defaults
    self.llm = environment.map { LLMSettingsViewModel(environment: $0) }
    self.obsidian = environment.map { ObsidianSettingsViewModel(environment: $0) }
  }

  /// Permissions, the retention sentence and the setup rows from the app's
  /// environment.
  convenience init(environment: AppEnvironment, defaults: UserDefaults = .standard) {
    self.init(
      permissions: environment.permissions, settings: environment.settings,
      environment: environment, defaults: defaults)
  }

  /// Whether the opener shows the window: a required permission is missing,
  /// or this install has not finished the two pages yet (the flag is unset).
  /// An install with the flag unset whose endpoint and vault are already in
  /// `settings` has nothing left to ask: the flag is written and the window
  /// stays closed, instead of opening on page 2 only to close at once.
  static func shouldOpen(
    permissions: any PermissionsChecking, settings: SettingsStore? = nil, defaults: UserDefaults
  ) async -> Bool {
    for kind in PermissionKind.allCases where kind.isRequired {
      if await permissions.state(of: kind) != .granted { return true }
    }
    guard !defaults.bool(forKey: onboardingCompletedKey) else { return false }
    if let settings, let stored = try? await settings.load(), stored.llmConfigured,
      stored.vaultConfigured
    {
      defaults.set(true, forKey: onboardingCompletedKey)
      return false
    }
    return true
  }

  func load() async {
    for index in steps.indices {
      steps[index].state = await permissions.state(of: steps[index].kind)
    }
    if let settings, let stored = try? await settings.load() {
      retentionSentence = Self.retentionSentence(for: stored)
    }
    // The page 2 rows load once: a later `load()` ("Check again" on page 1)
    // refreshes the permissions and keeps whatever was typed on page 2.
    guard !loaded else { return }
    loaded = true
    if let llm {
      await llm.load()
      if llm.isConfigured { setupStates[.summaries] = .saved(Self.savedLine(llm)) }
    }
    if let obsidian {
      await obsidian.load()
      if obsidian.enabled { setupStates[.vault] = .saved(Self.savedLine(obsidian)) }
    }
    // An install that has the permissions but never saw page 2 (the flag is
    // unset) starts there; a fresh install starts on page 1.
    if isComplete {
      page = .setup
      finishIfSetupHandled()
    }
  }

  /// One sentence on what the rule does to the files, then where to change
  /// it: Settings > Recording, the section that holds the rule. The days
  /// and delete sentences are that section's footnotes.
  static func retentionSentence(for settings: Settings) -> String {
    let rule =
      switch settings.defaultRetention {
      case .keepForever:
        "Recordings are kept until you delete them."
      case .keepDays, .deleteAfterProcessing:
        settings.defaultRetention.footnote
      }
    return rule + " Change this any time in Settings > Recording."
  }

  // MARK: - Page 1

  /// The first step that is neither granted nor skipped; the last one when
  /// every step is handled.
  var current: PermissionKind {
    steps.first { $0.state != .granted && !skipped.contains($0.kind) }?.kind ?? .localNetwork
  }

  var isComplete: Bool {
    steps.filter(\.isRequired).allSatisfy { $0.state == .granted }
  }

  /// Every permission step handled: required ones granted, optional ones
  /// granted or skipped. Page 1 advances on its own when this turns true.
  var permissionsHandled: Bool {
    isComplete
      && steps.filter { !$0.isRequired }.allSatisfy {
        $0.state == .granted || skipped.contains($0.kind)
      }
  }

  /// Both setup rows saved or skipped. On page 2 this finishes the window
  /// (`finishIfSetupHandled`).
  var setupHandled: Bool {
    SetupStep.allCases.allSatisfy { setupState(of: $0).isHandled }
  }

  func state(of kind: PermissionKind) -> PermissionState {
    steps.first { $0.kind == kind }?.state ?? .unknown
  }

  func request(_ kind: PermissionKind) async {
    requesting = kind
    let state = await permissions.request(kind)
    if let index = steps.firstIndex(where: { $0.kind == kind }) {
      steps[index].state = state
    }
    requesting = nil
  }

  func openSystemSettings(_ kind: PermissionKind) {
    permissions.openSystemSettings(for: kind)
  }

  /// Optional steps can be skipped; required ones cannot.
  func skip(_ kind: PermissionKind) {
    guard !kind.isRequired else { return }
    skipped.insert(kind)
  }

  /// Continue or Later on page 1. Page 2 with both rows already handled (an
  /// install configured in Settings) has nothing to show, so it finishes.
  func advance() {
    page = .setup
    finishIfSetupHandled()
  }

  /// Back on page 2.
  func back() {
    page = .permissions
  }

  // MARK: - Page 2

  func setupState(of step: SetupStep) -> SetupState {
    setupStates[step] ?? .open
  }

  func skipSetup(_ step: SetupStep) {
    setupStates[step] = .skipped
    finishIfSetupHandled()
  }

  /// The Summaries row's Save can go: a valid URL, a model name and nothing
  /// the view model rejects. The ChatGPT choice has no Save: its consent
  /// button is the save (`confirmSummariesWithCodex`).
  var canSaveSummaries: Bool {
    guard let llm, llm.preset != .codex else { return false }
    return llm.validationMessage == nil && llm.baseURL != nil
      && !llm.model.trimmingCharacters(in: .whitespaces).isEmpty
  }

  /// Saves through the LLM tab's view model (same validation, same pipeline
  /// rebuild) and collapses the row on success.
  func saveSummaries() async {
    guard let llm, canSaveSummaries else { return }
    await llm.save()
    guard llm.error == nil, llm.isConfigured else { return }
    setupStates[.summaries] = .saved(Self.savedLine(llm))
    finishIfSetupHandled()
  }

  /// The consent card's button on the Summaries row: the LLM tab's own
  /// `confirmCodex()` (stores the confirmation and provider, picks the
  /// first listed model, saves, probes), then the row collapses when the
  /// endpoint is configured.
  func confirmSummariesWithCodex() async {
    guard let llm, llm.preset == .codex else { return }
    await llm.confirmCodex()
    guard llm.error == nil, llm.isConfigured else { return }
    setupStates[.summaries] = .saved(Self.savedLine(llm))
    finishIfSetupHandled()
  }

  /// Saves through the Obsidian tab's view model (validated by the
  /// destination; `ObsidianError` verbatim in `obsidian.validationMessage`)
  /// and collapses the row on success.
  /// A folder from the chooser becomes the vault and is saved at once.
  func chooseVault(_ url: URL) async {
    obsidian?.vaultPath = url.path
    await saveVault()
  }

  func saveVault() async {
    guard let obsidian else { return }
    obsidian.enabled = true
    await obsidian.save()
    guard obsidian.saved, obsidian.error == nil else { return }
    setupStates[.vault] = .saved(Self.savedLine(obsidian))
    finishIfSetupHandled()
  }

  // MARK: - Exit

  /// The flag alone, for the window's close button: the opener no longer
  /// shows the window for the flag, only for a missing required permission.
  func markCompleted() {
    defaults.set(true, forKey: Self.onboardingCompletedKey)
  }

  /// Finish, or both rows handled on page 2: the flag, then `finished`.
  func finish() {
    markCompleted()
    finished = true
  }

  private func finishIfSetupHandled() {
    if page == .setup, setupHandled { finish() }
  }

  private static func savedLine(_ llm: LLMSettingsViewModel) -> String {
    if llm.preset == .codex {
      let account: String =
        if case .signedIn(let line) = llm.codexStatus { " as \(line)" } else { "" }
      return "Saved: \(llm.codexModel) via ChatGPT\(account)"
    }
    let host = llm.baseURL?.host() ?? ""
    return "Saved: \(llm.model.trimmingCharacters(in: .whitespaces)) at \(host)"
  }

  private static func savedLine(_ obsidian: ObsidianSettingsViewModel) -> String {
    "Saved: \(URL(fileURLWithPath: obsidian.vaultPath).lastPathComponent)"
  }
}
