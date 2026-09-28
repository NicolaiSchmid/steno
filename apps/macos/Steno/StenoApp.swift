import AppKit
import StenoAudio
import SwiftUI

/// Scenes: the main window, the onboarding window, the menu bar item and
/// Settings. Every scene renders over `AppBootstrap.shared`, which builds
/// the environment asynchronously at launch (`live()`, or `preview()` when
/// launched with `-steno-ui-testing`).
@main
struct StenoApp: App {
  @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
  private let bootstrap = AppBootstrap.shared

  init() {
    let bootstrap = self.bootstrap
    Task { @MainActor in await bootstrap.load() }
  }

  var body: some Scene {
    // No title bar: the window carries no title and no toolbar items; each
    // column of `MainWindow` paints its own opaque background up to the top
    // edge, so nothing but the traffic lights sits above the content.
    Window("Steno", id: "main") {
      RootView(bootstrap: bootstrap) { controller in
        MainWindow(controller: controller)
          .modifier(OnboardingOpener(controller: controller))
      }
    }
    .windowStyle(.hiddenTitleBar)
    .defaultSize(width: 1120, height: 720)
    .commands { AppCommands(bootstrap: bootstrap) }

    Window("Welcome to Steno", id: "onboarding") {
      RootView(bootstrap: bootstrap) { controller in
        OnboardingWindowContent(controller: controller)
      }
    }
    .windowResizability(.contentSize)

    MenuBarExtra {
      RootView(bootstrap: bootstrap) { controller in
        MenuBarView(controller: controller)
      }
    } label: {
      MenuBarLabel(bootstrap: bootstrap)
    }
    .menuBarExtraStyle(.window)

    Settings {
      RootView(bootstrap: bootstrap) { controller in
        SettingsView(controller: controller)
      }
    }
  }
}

/// Builds the environment once and holds the controller and the detection
/// panel presenter for the app's lifetime.
@MainActor
@Observable
final class AppBootstrap {
  static let shared = AppBootstrap()

  private(set) var controller: AppController?
  private(set) var error: String?
  private let panels = DetectionPanelPresenter()
  private var loading = false

  /// The `-steno-*` launch arguments, parsed once.
  static let scenario = UITestScenario(arguments: CommandLine.arguments)

  static var isUITesting: Bool { scenario.isUITesting }

  func load() async {
    guard controller == nil, !loading else { return }
    loading = true
    defer { loading = false }
    do {
      let environment: AppEnvironment
      if Self.isUITesting {
        environment = try await AppEnvironment.preview(seed: Self.scenario.seed)
      } else {
        environment = try await AppEnvironment.live(updater: UpdaterController())
      }
      let controller = AppController(environment: environment)
      controller.detection.promptDidChange = { [panels] prompt in panels.present(prompt) }
      self.controller = controller
      await controller.launch()
      if Self.isUITesting, Self.scenario.startsRecording {
        // As the sidebar control would: the live row becomes the selection.
        await controller.startRecordingFromWindow(mode: .call)
      }
    } catch {
      self.error = "Steno could not start: \(error)"
    }
  }
}

/// Spinner or error until the controller exists, then `content`.
struct RootView<Content: View>: View {
  let bootstrap: AppBootstrap
  @ViewBuilder let content: (AppController) -> Content

  var body: some View {
    if let controller = bootstrap.controller {
      content(controller)
    } else if let error = bootstrap.error {
      VStack(spacing: Theme.Space.sm) {
        MessageRow(kind: .error, text: error)
        Button("Quit") { NSApp.terminate(nil) }
      }
      .padding(Theme.Space.xl)
      .frame(minWidth: 320)
    } else {
      ProgressView()
        .controlSize(.small)
        .padding(Theme.Space.xl)
        .frame(minWidth: 320, minHeight: 120)
    }
  }
}

struct MenuBarLabel: View {
  let bootstrap: AppBootstrap

  var body: some View {
    let recording = bootstrap.controller?.recorder.isRecording ?? false
    Image(systemName: recording ? "record.circle.fill" : "waveform")
      .symbolRenderingMode(recording ? .multicolor : .monochrome)
      .accessibilityLabel(recording ? "Steno, recording" : "Steno")
  }
}

/// Opens the onboarding window at launch when a required permission is
/// missing or this install has not finished the two pages and is not
/// already configured in Settings (`OnboardingViewModel.shouldOpen`); never
/// in the preview environment.
struct OnboardingOpener: ViewModifier {
  let controller: AppController
  @Environment(\.openWindow) private var openWindow
  @State private var checked = false

  func body(content: Content) -> some View {
    content.task {
      guard !checked, !controller.environment.isPreview else { return }
      checked = true
      if await OnboardingViewModel.shouldOpen(
        permissions: controller.environment.permissions,
        settings: controller.environment.settings, defaults: .standard)
      {
        openWindow(id: "onboarding")
      }
    }
  }
}

struct OnboardingWindowContent: View {
  let controller: AppController
  @Environment(\.dismissWindow) private var dismissWindow

  var body: some View {
    OnboardingView(
      model: OnboardingViewModel(environment: controller.environment),
      onFinished: { dismissWindow(id: "onboarding") })
  }
}

@MainActor
struct AppCommands: Commands {
  let bootstrap: AppBootstrap
  /// Published by the list column while the main window is key; ⌘F runs it.
  @FocusedValue(\.searchFocus) private var searchFocus

  /// The same state table the sidebar control and the menu bar item render;
  /// without a controller the menu reads as idle and is disabled.
  private var presentation: RecordingControlPresentation {
    guard let recorder = bootstrap.controller?.recorder else { return .unavailable }
    return RecordingControlPresentation.make(
      state: recorder.recording, denied: recorder.deniedPermissions)
  }

  var body: some Commands {
    let presentation = self.presentation
    CommandGroup(after: .appInfo) {
      Button("Check for Updates…") { bootstrap.controller?.menuBar.checkForUpdates() }
        .disabled(bootstrap.controller == nil)
    }
    // Before the system's Find submenu, so ⌘F reaches the meeting search
    // whenever the main window is key.
    CommandGroup(before: .textEditing) {
      Button("Find Meetings") { searchFocus?.run() }
        .keyboardShortcut("f", modifiers: .command)
        .disabled(searchFocus == nil)
    }
    CommandMenu("Record") {
      Button(presentation.menuLabel) {
        guard let controller = bootstrap.controller else { return }
        Task { await controller.recorder.toggleRecording() }
      }
      .keyboardShortcut("r", modifiers: [.command, .shift])
      .disabled(!presentation.isEnabled)
      Button("Record In Person") {
        guard let controller = bootstrap.controller else { return }
        Task { await controller.recorder.start(mode: .inPerson) }
      }
      .disabled(!presentation.offersInPerson)
    }
    #if DEBUG
      CommandMenu("Debug") {
        // The audio workstream's S1 check from a bundled, signed app: runs
        // the real tap pipeline against `afplay` and reports signal versus
        // silence in the menu bar item.
        Button("Run System Audio Probe") {
          Task {
            let granted = await SystemAudioPermission.request()
            bootstrap.controller?.recorder.noteProbe(granted: granted)
          }
        }
        .disabled(bootstrap.controller == nil)
      }
    #endif
  }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
  func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
    // The menu bar item keeps running when the window closes.
    false
  }

  func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
    guard let controller = AppBootstrap.shared.controller else { return .terminateNow }
    Task { @MainActor in
      await controller.shutdown()
      NSApp.reply(toApplicationShouldTerminate: true)
    }
    return .terminateLater
  }
}
