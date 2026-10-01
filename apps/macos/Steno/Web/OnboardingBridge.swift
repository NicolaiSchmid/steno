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
    // Armed after the load, which writes across many awaits; `page.ready`
    // before this is a no-op flush and `start()` then publishes once.
    await model.load()
    start()
    await BridgeHostSupport.untilCancelled()
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
    // The Summaries form sends the Settings window's method names; this
    // window answers them on its own model.
    if let llm = model.llm, try await SummariesCommands.handle(request, llm: llm) { return nil }
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
    case .onboardingSkip:
      model.skip(try permission(request))
    case .onboardingRefresh:
      await model.load()
    case .onboardingAdvance:
      model.advance()
    case .onboardingBack:
      model.back()

    case .onboardingSaveSummaries:
      await model.saveSummaries()
    case .onboardingConfirmSummariesWithCodex:
      await model.confirmSummariesWithCodex()

    case .onboardingChooseVault:
      let obsidian = try requireObsidian()
      let chosen = await chooseFolder(obsidian.vaultURL)
      if let chosen { await model.chooseVault(chosen) }
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

  private func permission(_ request: BridgeRequest) throws -> PermissionKind {
    try BridgeSystemCommands.permissionKind(try request.params(PermissionKindParams.self).kind)
  }

  private func requireObsidian() throws -> ObsidianSettingsViewModel {
    guard let obsidian = model.obsidian else {
      throw BridgeError(code: .failed, message: "This window has no export settings to write.")
    }
    return obsidian
  }
}
