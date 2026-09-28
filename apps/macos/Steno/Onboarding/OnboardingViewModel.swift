import Foundation
import StenoCore

/// Onboarding in two pages. Page 1, permissions: microphone, system audio
/// (both required), then calendar (optional) and local network (optional;
/// the prompt comes with the first phone pairing, so this step only
/// explains); under the intro, one sentence says what happens to the
/// recordings under the stored retention rule. Page 2, "Summaries and
/// export": the LLM endpoint and the Obsidian vault, both optional, written
/// through the same `LLMSettingsViewModel` and `ObsidianSettingsViewModel`
/// the Settings tabs use. Finishing by any route sets
/// `steno.onboardingCompleted`; installs without the flag open once more,
/// straight on page 2 when the permissions are already granted.
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

    var title: String {
      switch self {
      case .summaries: "Summaries"
      case .vault: "Obsidian vault"
      }
    }

    var explanation: String {
      switch self {
      case .summaries:
        "Steno sends the transcript text, never audio, to an OpenAI-compatible endpoint to clean it up and write the summary, tasks and decisions. Without one, meetings keep a raw transcript and no summary."
      case .vault:
        "Steno writes each meeting into Meetings/<date>-<slug>/ inside the vault: a folder note, transcript, tasks, VTT and JSON. It never touches files it did not write. Without a vault, meetings stay in Steno."
      }
    }

    /// Where the fields onboarding leaves out live.
    var footnote: String {
      switch self {
      case .summaries: "The context window and the rest live in Settings > LLM."
      case .vault: "People pages, the task tag and the audio copy live in Settings > Obsidian."
      }
    }
  }

  enum SetupState: Equatable, Sendable {
    case open
    /// Saved, with the collapsed row's line ("Saved: <model> at <host>").
    case saved(String)
    case skipped

    var isHandled: Bool { self != .open }
  }

  /// Every row of the window in order: the permission steps, then the two
  /// setup steps.
  enum StepKind: Equatable, Sendable {
    case permission(PermissionKind)
    case summaries
    case vault
  }

  static let onboardingCompletedKey = "steno.onboardingCompleted"

  private(set) var steps = PermissionKind.allCases.map { Step(kind: $0, state: .unknown) }
  private(set) var requesting: PermissionKind?
  private(set) var skipped: Set<PermissionKind> = []
  private(set) var page: Page = .permissions
  private(set) var setupStates: [SetupStep: SetupState] = [.summaries: .open, .vault: .open]
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

  /// The order of every row in the window.
  var stepOrder: [StepKind] {
    steps.map { .permission($0.kind) } + [.summaries, .vault]
  }

  /// Whether the opener shows the window: a required permission is missing,
  /// or this install has not finished the two pages yet (the flag is unset).
  static func shouldOpen(permissions: any PermissionsChecking, defaults: UserDefaults) async
    -> Bool
  {
    guard defaults.bool(forKey: onboardingCompletedKey) else { return true }
    for kind in PermissionKind.allCases where kind.isRequired {
      if await permissions.state(of: kind) != .granted { return true }
    }
    return false
  }

  func load() async {
    for index in steps.indices {
      steps[index].state = await permissions.state(of: steps[index].kind)
    }
    if let settings, let stored = try? await settings.load() {
      retentionSentence = Self.retentionSentence(for: stored)
    }
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
    if !loaded, isComplete { page = .setup }
    loaded = true
  }

  /// One sentence on what the rule does to the files, then where to change
  /// it. The days and delete sentences are the Audio tab's footnotes.
  static func retentionSentence(for settings: Settings) -> String {
    let rule =
      switch settings.defaultRetention {
      case .keepForever:
        "Recordings are kept forever in \(settings.audioFolder.lastPathComponent)."
      case .keepDays, .deleteAfterProcessing:
        settings.defaultRetention.footnote
      }
    return rule + " Change this any time in Settings > Audio."
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

  /// Both setup rows saved or skipped. The window closes when this turns
  /// true on page 2.
  var setupHandled: Bool {
    SetupStep.allCases.allSatisfy { setupStates[$0]?.isHandled ?? false }
  }

  /// Every row handled: the permissions and the two setup steps.
  var isFinished: Bool {
    permissionsHandled && setupHandled
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

  /// Done or Later on page 1.
  func advance() {
    page = .setup
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
  }

  /// The Summaries row's Save can go: a valid URL, a model name and nothing
  /// the view model rejects.
  var canSaveSummaries: Bool {
    guard let llm else { return false }
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
  }

  /// Saves through the Obsidian tab's view model (validated by the
  /// destination; `ObsidianError` verbatim in `obsidian.validationMessage`)
  /// and collapses the row on success.
  func saveVault() async {
    guard let obsidian else { return }
    obsidian.enabled = true
    await obsidian.save()
    guard obsidian.saved, obsidian.error == nil else { return }
    setupStates[.vault] = .saved(Self.savedLine(obsidian))
  }

  /// Finishing by any route: the opener no longer shows the window for the
  /// flag alone.
  func markCompleted() {
    defaults.set(true, forKey: Self.onboardingCompletedKey)
  }

  private static func savedLine(_ llm: LLMSettingsViewModel) -> String {
    let host = llm.baseURL?.host() ?? ""
    return "Saved: \(llm.model.trimmingCharacters(in: .whitespaces)) at \(host)"
  }

  private static func savedLine(_ obsidian: ObsidianSettingsViewModel) -> String {
    "Saved: \(URL(fileURLWithPath: obsidian.vaultPath).lastPathComponent)"
  }
}

extension PermissionKind {
  var title: String {
    switch self {
    case .microphone: "Microphone"
    case .systemAudio: "System audio"
    case .calendar: "Calendar"
    case .localNetwork: "Local network"
    }
  }

  var explanation: String {
    switch self {
    case .microphone:
      "Steno records your side of a meeting from the microphone. Required."
    case .systemAudio:
      "Steno records the other side from the apps playing audio on this Mac. macOS asks once, during a short test recording. Required."
    case .calendar:
      "Steno names recordings after the calendar event they overlap and suggests the attendees as speakers. Optional."
    case .localNetwork:
      "The Steno iPhone app sends recordings over your Wi-Fi. macOS asks when you pair the first phone. Optional."
    }
  }
}
