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

  /// 1120 x 720, or `-steno-window WxH` under UI testing.
  @MainActor private static var mainWindowSize: CGSize {
    if AppBootstrap.isUITesting, let size = AppBootstrap.scenario.windowSize {
      return CGSize(width: size.width, height: size.height)
    }
    return CGSize(width: 1120, height: 720)
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
      .background {
        if AppBootstrap.isUITesting { UITestWindowSizer() }
      }
    }
    .windowStyle(.hiddenTitleBar)
    .defaultSize(Self.mainWindowSize)
    .commands { AppCommands(bootstrap: bootstrap) }

    // No title bar: the H1 inside is the window's one title.
    Window("Welcome to Steno", id: "onboarding") {
      RootView(bootstrap: bootstrap) { controller in
        OnboardingWindowContent(controller: controller)
      }
    }
    .windowStyle(.hiddenTitleBar)
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
/// panel presenter for the app's lifetime. `openMain` is installed by
/// `MenuBarLabel`, a view inside the one scene that is never torn down,
/// because the panel's content is outside every scene and an `openWindow`
/// read there does nothing.
@MainActor
@Observable
final class AppBootstrap {
  static let shared = AppBootstrap()

  private(set) var controller: AppController?
  private(set) var error: String?
  private let panels = FloatingPanelPresenter()
  /// Ticks once a second while recording; the bubble and the menu bar label
  /// read it for the elapsed time.
  let clock = RecordingClock()
  private var loading = false
  /// Opens the main window and activates Steno; replaced by a scene's
  /// `openWindow` as soon as one renders. Until then, activation alone.
  var openMain: @MainActor () -> Void = { NSApp.activate() }

  /// The `-steno-*` launch arguments, parsed once.
  static let scenario = UITestScenario(arguments: CommandLine.arguments)

  static var isUITesting: Bool { scenario.isUITesting }

  func load() async {
    guard controller == nil, !loading else { return }
    loading = true
    defer { loading = false }
    if let launchError = Self.scenario.launchError {
      self.error = launchError
      return
    }
    do {
      let environment: AppEnvironment
      if Self.isUITesting {
        // The scenario's seed set; every permission unknown under
        // `-steno-show-onboarding`, so the window opens on page 1 with its
        // rows open.
        environment = try await AppEnvironment.preview(
          seed: Self.scenario.seed,
          permissions: Self.scenario.showsOnboarding ? FakePermissions() : nil)
      } else {
        environment = try await AppEnvironment.live(updater: UpdaterController())
      }
      let controller = AppController(environment: environment)
      self.controller = controller
      await controller.launch()
      panels.follow(controller, clock: clock) { [weak self] in self?.openMain() }
      if Self.isUITesting, Self.scenario.showPrompt {
        controller.detection.appName = { _ in "Zoom" }
        await controller.detection.handle(.microphoneOpened(bundleID: "us.zoom.xos", pid: 1))
      }
      if Self.isUITesting, Self.scenario.startsRecording {
        // As the sidebar control would: the live row becomes the selection
        // and the detail header shows its Stop control.
        await controller.startRecordingFromWindow(mode: .call)
      }
      if Self.isUITesting, let section = Self.scenario.settingsSection {
        // The setup banner's deep link: `SettingsView` selects the section
        // when its window opens (the test opens it from the nav column's
        // Settings row).
        controller.openSettings(section)
      }
    } catch {
      self.error = "Steno could not start: \(error)"
    }
  }
}

/// Installs the scene's `openWindow` as `AppBootstrap.openMain`, so the
/// floating bubble can bring the main window forward. Applied from the
/// `MenuBarExtra` label only: a `Window` scene's `openWindow` would be the
/// last writer and that window is the one the user closes.
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

extension View {
  /// Installs this scene's `openWindow` as `bootstrap.openMain`.
  func installsOpenMain(_ bootstrap: AppBootstrap) -> some View {
    modifier(OpenMainInstaller(bootstrap: bootstrap))
  }
}

/// Applies `-steno-window WxH` to the main window once its `NSWindow`
/// exists, and once more a second later: SwiftUI restores the frame a
/// previous launch saved over `defaultSize`, and the smoke suite launches
/// the app many times per run. Zero-sized, in the window content's
/// background under UI testing only; without the flag it does nothing.
struct UITestWindowSizer: NSViewRepresentable {
  func makeNSView(context: Context) -> SizerView {
    SizerView(frame: .zero)
  }

  func updateNSView(_ nsView: SizerView, context: Context) {}

  final class SizerView: NSView {
    override func viewDidMoveToWindow() {
      super.viewDidMoveToWindow()
      UITestWindowSizer.apply(to: window)
      Task { @MainActor [weak self] in
        try? await Task.sleep(for: .seconds(1))
        UITestWindowSizer.apply(to: self?.window)
      }
    }
  }

  /// Sets the content size and centres the window when the scenario asks
  /// for a size the window does not already have.
  @MainActor static func apply(to window: NSWindow?) {
    guard AppBootstrap.isUITesting, let size = AppBootstrap.scenario.windowSize, let window
    else { return }
    let content = NSSize(width: size.width, height: size.height)
    guard window.contentRect(forFrameRect: window.frame).size != content else { return }
    window.setContentSize(content)
    window.center()
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
/// busy, and the symbol plus the elapsed time while recording, re-rendered
/// by the shared `RecordingClock`'s tick (legible at a glance, immune to
/// template rendering, self-refreshing). Always alive, so it also installs
/// `openMain` for the floating bubble.
struct MenuBarLabel: View {
  let bootstrap: AppBootstrap

  var body: some View {
    let state = bootstrap.controller?.recorder.recording ?? .idle
    label(MenuBarLabelPresentation.make(state: state, now: bootstrap.clock.now))
      .installsOpenMain(bootstrap)
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
/// missing or this install has not finished the two pages and is not
/// already configured in Settings (`OnboardingViewModel.shouldOpen`); never
/// in the preview environment, except under `-steno-show-onboarding`, which
/// opens it outright so the smoke test can read the pages.
struct OnboardingOpener: ViewModifier {
  let controller: AppController
  @Environment(\.openWindow) private var openWindow
  @State private var checked = false

  func body(content: Content) -> some View {
    content.task {
      guard !checked else { return }
      checked = true
      if controller.environment.isPreview {
        if AppBootstrap.isUITesting, AppBootstrap.scenario.showsOnboarding {
          openWindow(id: "onboarding")
        }
        return
      }
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
  /// `-steno-appearance light|dark`: the whole app renders in that
  /// appearance whatever the runner's system setting, so the smoke test can
  /// screenshot both.
  func applicationWillFinishLaunching(_ notification: Notification) {
    guard AppBootstrap.isUITesting, let appearance = AppBootstrap.scenario.appearance else {
      return
    }
    let name: NSAppearance.Name = appearance == .dark ? .darkAqua : .aqua
    NSApplication.shared.appearance = NSAppearance(named: name)
  }

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
