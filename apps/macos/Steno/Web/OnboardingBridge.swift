import AppKit
import Foundation
import StenoBridge
import StenoCore

/// The onboarding window's host (plan Decisions 5 and 6). Owns the
/// `OnboardingViewModel` (and through it the Summaries and Export view
/// models page 2 writes), loads it when the window opens and maps it onto
/// the one `onboarding` topic; `TopicPublisher` follows it and publishes. Commands map one
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
final class OnboardingBridge: BridgeHost, TopicSource {
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
  private lazy var publisher = TopicPublisher(topics: [.onboarding], source: self)

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
    publisher.start()
  }

  /// Runs for the window's lifetime (`OnboardingWindow`'s `.task`): loads
  /// the model, then stays alive so later changes keep publishing, until
  /// the window closes and cancels it.
  func run() async {
    // Armed after the load, which writes across many awaits; `page.ready`
    // before this is a no-op flush and `start()` then publishes once.
    await model.load()
    start()
    await TopicPublisher.untilCancelled()
    stop()
  }

  func stop() {
    publisher.stop()
  }

  func attach(_ events: any BridgeEventSink) {
    publisher.attach(events)
  }

  // MARK: - Topics

  func snapshot(for topic: BridgeTopic) -> (any Encodable)? {
    guard topic == .onboarding else { return nil }
    return OnboardingSnapshot(model: model)
  }

  func didPublish(_ topic: BridgeTopic, emitted: Bool) {
    guard emitted else { return }
    UITestDiagnostics.note(
      "onboarding published page \(model.page == .permissions ? "permissions" : "setup")")
  }

  // MARK: - Commands

  func handle(_ request: BridgeRequest) async throws -> JSONValue? {
    UITestDiagnostics.note("onboarding handles \(request.method.rawValue)")
    // The Summaries form sends the Settings window's method names; this
    // window answers them on its own model.
    if let llm = model.llm, try await SummariesCommands.handle(request, llm: llm) { return nil }
    switch request.method {
    case .pageReady:
      UITestDiagnostics.note("onboarding page ready")
      publisher.pageDidBecomeReady()
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

  /// Page 1 moves on by itself once every step is handled (required ones
  /// granted, optional ones granted or skipped), as the SwiftUI page did on
  /// that change. A window rule, not the model's: the model's own tests pin
  /// that page 1 never finishes by itself. Only after a command that can
  /// change a step, so Back from page 2 stays on page 1.
  private func advanceIfHandled() {
    if model.page == .permissions, model.permissionsHandled { model.advance() }
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
