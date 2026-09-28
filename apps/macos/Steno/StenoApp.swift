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
    Window("Steno", id: "main") {
      RootView(bootstrap: bootstrap) { controller in
        MainWindow(controller: controller)
          .modifier(OnboardingOpener(controller: controller))
      }
    }
    .defaultSize(width: 1040, height: 680)
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

/// Builds the environment once and holds the controller and the floating
/// panel presenter for the app's lifetime. `openMain` is installed by a view
/// inside a scene (`RootView`, `MenuBarLabel`), because the panel's content
/// is outside every scene and an `openWindow` read there does nothing.
@MainActor
@Observable
final class AppBootstrap {
  static let shared = AppBootstrap()

  private(set) var controller: AppController?
  private(set) var error: String?
  private let panels = FloatingPanelPresenter()
  private var loading = false
  /// Opens the main window and activates Steno; replaced by a scene's
  /// `openWindow` as soon as one renders. Until then, activation alone.
  var openMain: @MainActor () -> Void = { NSApp.activate() }

  static let scenario = UITestScenario(arguments: CommandLine.arguments)

  static var isUITesting: Bool { scenario.isUITesting }

  func load() async {
    guard controller == nil, !loading else { return }
    loading = true
    defer { loading = false }
    do {
      let environment: AppEnvironment
      if Self.isUITesting {
        environment = try await AppEnvironment.preview()
      } else {
        environment = try await AppEnvironment.live(updater: UpdaterController())
      }
      let controller = AppController(environment: environment)
      self.controller = controller
      await controller.launch()
      panels.follow(controller) { [weak self] in self?.openMain() }
      if Self.isUITesting, Self.scenario.showPrompt {
        controller.detection.appName = { _ in "Zoom" }
        await controller.detection.handle(.microphoneOpened(bundleID: "us.zoom.xos", pid: 1))
      }
    } catch {
      self.error = "Steno could not start: \(error)"
    }
  }
}

/// Installs the scene's `openWindow` as `AppBootstrap.openMain`, so the
/// floating bubble can bring the main window forward.
struct OpenMainInstaller: ViewModifier {
  let bootstrap: AppBootstrap
  @Environment(\.openWindow) private var openWindow

  func body(content: Content) -> some View {
    content.onAppear {
      let openWindow = self.openWindow
      bootstrap.openMain = {
        openWindow(id: "main")
        NSApp.activate()
      }
    }
  }
}

/// Spinner or error until the controller exists, then `content`.
struct RootView<Content: View>: View {
  let bootstrap: AppBootstrap
  @ViewBuilder let content: (AppController) -> Content

  var body: some View {
    Group {
      if let controller = bootstrap.controller {
        content(controller)
      } else if let error = bootstrap.error {
        failure(error)
      } else {
        ProgressView()
          .controlSize(.small)
          .padding(Theme.Space.xl)
          .frame(minWidth: 320, minHeight: 120)
      }
    }
    .modifier(OpenMainInstaller(bootstrap: bootstrap))
  }

  private func failure(_ error: String) -> some View {
    VStack(spacing: Theme.Space.sm) {
      MessageRow(kind: .error, text: error)
      Button("Quit") { NSApp.terminate(nil) }
    }
    .padding(Theme.Space.xl)
    .frame(minWidth: 320)
  }
}

/// The menu bar item's label: `waveform` idle, `record.circle.fill` while
/// busy, and the symbol plus the elapsed time while recording, ticking on
/// its own `TimelineView` (legible at a glance, immune to template
/// rendering, self-refreshing). Always alive, so it also installs
/// `openMain` for the floating bubble.
struct MenuBarLabel: View {
  let bootstrap: AppBootstrap

  var body: some View {
    let state = bootstrap.controller?.recorder.recording ?? .idle
    Group {
      if case .recording(let since) = state {
        TimelineView(.periodic(from: since, by: 1)) { context in
          label(MenuBarLabelPresentation.make(state: state, now: context.date))
        }
      } else {
        label(MenuBarLabelPresentation.make(state: state, now: .now))
      }
    }
    .modifier(OpenMainInstaller(bootstrap: bootstrap))
  }

  private func label(_ presentation: MenuBarLabelPresentation) -> some View {
    HStack(spacing: Theme.Space.xs) {
      Image(systemName: presentation.symbolName)
      if let elapsed = presentation.elapsedText {
        Text(elapsed).monospacedDigit()
      }
    }
    .accessibilityLabel(presentation.accessibilityLabel)
  }
}

/// Opens the onboarding window at launch when a required permission is
/// missing (never in the preview environment).
struct OnboardingOpener: ViewModifier {
  let controller: AppController
  @Environment(\.openWindow) private var openWindow
  @State private var checked = false

  func body(content: Content) -> some View {
    content.task {
      guard !checked, !controller.environment.isPreview else { return }
      checked = true
      let permissions = controller.environment.permissions
      for kind in PermissionKind.allCases where kind.isRequired {
        if await permissions.state(of: kind) != .granted {
          openWindow(id: "onboarding")
          return
        }
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
