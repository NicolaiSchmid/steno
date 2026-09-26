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

  static var isUITesting: Bool {
    CommandLine.arguments.contains("-steno-ui-testing")
  }

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
      controller.detection.promptDidChange = { [panels] prompt in panels.present(prompt) }
      self.controller = controller
      await controller.launch()
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
      model: OnboardingViewModel(permissions: controller.environment.permissions),
      onFinished: { dismissWindow(id: "onboarding") })
  }
}

struct AppCommands: Commands {
  let bootstrap: AppBootstrap

  var body: some Commands {
    CommandGroup(after: .appInfo) {
      Button("Check for Updates…") { bootstrap.controller?.menuBar.checkForUpdates() }
        .disabled(bootstrap.controller == nil)
    }
    CommandMenu("Record") {
      Button(bootstrap.controller?.recorder.isRecording == true ? "Stop Recording" : "Record Call")
      {
        guard let controller = bootstrap.controller else { return }
        Task { await controller.recorder.toggleRecording() }
      }
      .keyboardShortcut("r", modifiers: [.command, .shift])
      .disabled(bootstrap.controller == nil)
      Button("Record In Person") {
        guard let controller = bootstrap.controller else { return }
        Task { await controller.recorder.start(mode: .inPerson) }
      }
      .disabled(bootstrap.controller?.recorder.recording != .idle)
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
