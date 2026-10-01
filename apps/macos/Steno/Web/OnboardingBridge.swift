import AppKit
import Foundation
import StenoBridge
import StenoCore

/// The onboarding window's host (plan Decisions 5 and 6). Owns the
/// `OnboardingViewModel` (and through it the Summaries and Export view
/// models page 2 writes), loads it when the window opens, follows it with
/// observation tracking and publishes the one `onboarding` snapshot to the
/// attached sink, coalesced to the next main-actor turn. Commands map one
/// to one onto the model's public methods, so no rule lives here; the two
/// pieces of view glue the SwiftUI page had stay glue: page 1 moves on by
/// itself once every permission step is handled, and the folder chooser
/// (`onboarding.chooseVault`) is the one native surface the page cannot
/// draw, an `NSOpenPanel` whose choice is saved as the vault at once.
///
/// The exit is the model's: `finish()` (or both setup rows handled) sets
/// `finished`, the page sees it in the snapshot and calls
/// `window.close(onboarding)`, which runs `closeWindow`.
@MainActor
final class OnboardingBridge: BridgeHost {
  /// What the page asks the host to open; `OnboardingWindow` installs the
  /// scene's `openWindow` here.
  typealias OpenWindow = @MainActor (WindowParams) -> Void
  /// Dismisses this window; `OnboardingWindow` installs `dismissWindow`.
  typealias CloseWindow = @MainActor () -> Void
  /// The folder chooser: `NSOpenPanel` in the app, a stub in tests.
  typealias ChooseFolder = SettingsBridge.ChooseFolder

  let controller: AppController
  let model: OnboardingViewModel
  var openWindow: OpenWindow = { _ in }
  var closeWindow: CloseWindow = {}

  private let chooseFolder: ChooseFolder
  private weak var events: (any BridgeEventSink)?
  /// True from `page.ready`: before it the page has no `window.steno` and
  /// an emit would be lost, so tracking is armed but nothing is sent.
  private var pageReady = false
  /// True between `start()` and `stop()`.
  private var running = false
  /// A publish is already scheduled for a later turn.
  private var pending = false

  /// Over the controller's environment, or over `model` when a test brings
  /// its own permissions and defaults.
  init(
    controller: AppController, model: OnboardingViewModel? = nil,
    chooseFolder: ChooseFolder? = nil
  ) {
    self.controller = controller
    self.model = model ?? OnboardingViewModel(environment: controller.environment)
    let panel: ChooseFolder = { await SettingsBridge.presentFolderPanel(startingAt: $0) }
    self.chooseFolder = chooseFolder ?? panel
  }

  /// Arms the tracking and lets later changes publish.
  func start() {
    guard !running else { return }
    running = true
    flush()
  }

  /// Runs for the window's lifetime (`OnboardingWindow`'s `.task`): loads
  /// the model, then stays alive so later changes keep publishing, until
  /// the window closes and cancels it.
  func run() async {
    start()
    await model.load()
    while !Task.isCancelled {
      try? await Task.sleep(for: .seconds(3600))
    }
    stop()
  }

  func stop() {
    running = false
  }

  func attach(_ events: any BridgeEventSink) {
    self.events = events
  }

  // MARK: - Publishing

  /// Asks for a publish on a later main-actor turn; a second ask before
  /// that turn is folded into it.
  private func schedule() {
    guard running, !pending else { return }
    pending = true
    Task { @MainActor [weak self] in self?.flush() }
  }

  /// Builds the snapshot inside observation tracking, so the next change to
  /// anything it read schedules the next publish, and emits it once the
  /// page is ready.
  private func flush() {
    guard running else { return }
    pending = false
    var snapshot: OnboardingSnapshot?
    withObservationTracking {
      snapshot = OnboardingSnapshot(model: self.model)
    } onChange: { [weak self] in
      Task { @MainActor [weak self] in self?.schedule() }
    }
    guard pageReady, let events, let snapshot else { return }
    events.emit(.onboarding, snapshot: snapshot)
  }

  // MARK: - Commands

  func handle(_ request: BridgeRequest) async throws -> JSONValue? {
    switch request.method {
    case .pageReady:
      UITestDiagnostics.note("onboarding page ready")
      pageReady = true
      flush()
    case .pageLayout:
      // Validated and dropped: the window has one size.
      _ = try request.params(PageLayoutParams.self)

    case .onboardingRequest:
      await model.request(try permission(request))
      advanceIfHandled()
    case .onboardingSkip:
      model.skip(try permission(request))
      advanceIfHandled()
    case .onboardingRefresh:
      await model.load()
      advanceIfHandled()
    case .onboardingAdvance:
      model.advance()
    case .onboardingBack:
      model.back()

    case .onboardingSelectPreset:
      let id = try request.params(SetStringParams.self).value
      guard let preset = LLMPreset(rawValue: id) else {
        throw BridgeError(code: .invalidParams, message: "Unknown summaries service \(id).")
      }
      let llm = try requireLLM()
      // Settings semantics, as the deleted picker had: the preset's address
      // and model are committed and probed at once when they validate.
      await llm.selectPreset(preset)
    case .onboardingUpdateSummaries:
      // The page keeps the draft while typing and sends the fields on blur;
      // the fields themselves are stored by `onboarding.saveSummaries`.
      let update = try request.params(SummariesUpdateParams.self)
      let llm = try requireLLM()
      if let baseURL = update.baseURL { llm.baseURLText = baseURL }
      if let model = update.model { llm.model = model }
      if let tokens = update.contextTokens { llm.contextTokensText = tokens }
      if let key = update.apiKey { llm.apiKey = key }
    case .onboardingTestSummaries:
      let llm = try requireLLM()
      await llm.test()
    case .onboardingSaveSummaries:
      await model.saveSummaries()
    case .onboardingConfirmSummariesWithCodex:
      await model.confirmSummariesWithCodex()
    case .onboardingRefreshCodexStatus:
      let llm = try requireLLM()
      await llm.refreshCodexStatus()

    case .onboardingChooseVault:
      let obsidian = try requireObsidian()
      let chosen = await chooseFolder(obsidian.vaultURL)
      if let chosen {
        obsidian.vaultPath = chosen.path
        await model.saveVault()
      }
      return try BridgeReplies.value(ChosenPathReply(path: chosen?.path))
    case .onboardingSaveVault:
      await model.saveVault()
    case .onboardingSkipSetup:
      let step = try request.params(SetupStepParams.self).step
      model.skipSetup(step == .summaries ? .summaries : .vault)
    case .onboardingFinish:
      model.finish()

    case .systemOpenURL:
      try BridgeSystemCommands.openURL(try request.params(OpenURLParams.self).url)
    case .systemOpenSystemSettings:
      model.openSystemSettings(try permission(request))
    case .windowOpen:
      openWindow(try request.params(WindowParams.self))
    case .windowClose:
      let target = try request.params(WindowParams.self)
      guard target.window == .onboarding else {
        throw BridgeError(
          code: .invalidParams, message: "The onboarding window closes only itself.")
      }
      closeWindow()

    default:
      throw BridgeError(
        code: .unknownMethod,
        message: "The onboarding window does not answer \(request.method.rawValue).")
    }
    return nil
  }

  /// Page 1 moves on by itself once every step is handled (required ones
  /// granted, optional ones granted or skipped), as the SwiftUI page did on
  /// that change. Only after a command that can change a step, so Back from
  /// page 2 stays on page 1.
  private func advanceIfHandled() {
    if model.page == .permissions, model.permissionsHandled { model.advance() }
  }

  private func permission(_ request: BridgeRequest) throws -> PermissionKind {
    try BridgeSystemCommands.permissionKind(try request.params(PermissionKindParams.self).kind)
  }

  private func requireLLM() throws -> LLMSettingsViewModel {
    guard let llm = model.llm else {
      throw BridgeError(code: .failed, message: "This window has no summaries settings to write.")
    }
    return llm
  }

  private func requireObsidian() throws -> ObsidianSettingsViewModel {
    guard let obsidian = model.obsidian else {
      throw BridgeError(code: .failed, message: "This window has no export settings to write.")
    }
    return obsidian
  }
}
