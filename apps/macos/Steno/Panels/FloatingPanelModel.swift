import AppKit
import Foundation
import Observation
import SwiftUI

/// The content the panel's SwiftUI root renders. The model writes it, the
/// root crossfades between the prompt and the bubble. It keeps the last
/// content while the panel fades out, so the exit is opacity only.
@MainActor
@Observable
final class FloatingPanelState {
  var content: FloatingContent?
}

/// The panel's rules over a `PanelHost`: content resolution, anchor load,
/// save and re-validation, and the observation-to-apply sequence. Pure over
/// the host, so anchor persistence and screen changes are unit tests
/// against a fake.
@MainActor
final class FloatingPanelModel {
  static let anchorKey = "steno.floatingPanel.anchor"

  let state = FloatingPanelState()
  private let host: any PanelHost
  private let defaults: UserDefaults
  private(set) var anchor: PanelAnchor?
  private var lastSize: CGSize?

  init(host: any PanelHost, defaults: UserDefaults) {
    self.host = host
    self.defaults = defaults
  }

  private var fallbackScreen: CGRect { host.currentScreens.first ?? .zero }

  /// The anchor to lay out from: the saved one while it lies on a current
  /// screen, else the default on the main screen.
  func currentAnchor() -> PanelAnchor {
    let resolved = PanelAnchor.validated(
      anchor ?? loadAnchor(), screens: host.currentScreens, fallback: fallbackScreen)
    anchor = resolved
    return resolved
  }

  /// Nil hides the panel; content shows it at the anchor, sized to fit.
  func apply(_ content: FloatingContent?) {
    guard let content else {
      host.hide()
      return
    }
    if state.content != content {
      state.content = content
    }
    place(size: host.contentFittingSize)
  }

  /// The user dragged the panel: the anchor follows and is saved. A move
  /// that arrives with a size other than the one last placed is AppKit
  /// resizing the window to its content; `panelDidResize` re-anchors that
  /// and the anchor must not follow it.
  func panelDidMove(to frame: CGRect) {
    guard let lastSize, Self.matches(frame.size, lastSize) else { return }
    let moved = PanelAnchor.from(
      frame: frame, screens: host.currentScreens, fallback: fallbackScreen)
    guard moved != anchor else { return }
    anchor = moved
    saveAnchor(moved)
  }

  /// The content resized the panel: keep the top-centre point.
  func panelDidResize(to size: CGSize) {
    guard host.isShown else { return }
    if let lastSize, Self.matches(size, lastSize) { return }
    place(size: size)
  }

  /// A display appeared or disappeared: a saved anchor on a screen that is
  /// gone falls back to the default anchor on the remaining screen, and the
  /// stale anchor is replaced in the defaults.
  func screensDidChange() {
    let before = anchor
    let resolved = PanelAnchor.validated(
      anchor ?? loadAnchor(), screens: host.currentScreens, fallback: fallbackScreen)
    anchor = resolved
    if let before, resolved != before { saveAnchor(resolved) }
    guard host.isShown, let lastSize else { return }
    host.show(frame: resolved.frame(for: lastSize))
  }

  /// AppKit may round a frame to the pixel grid; a difference under a point
  /// is the same size.
  private static func matches(_ lhs: CGSize, _ rhs: CGSize) -> Bool {
    abs(lhs.width - rhs.width) < 1 && abs(lhs.height - rhs.height) < 1
  }

  private func place(size: CGSize) {
    lastSize = size
    host.show(frame: currentAnchor().frame(for: size))
  }

  private func loadAnchor() -> PanelAnchor? {
    guard let data = defaults.data(forKey: Self.anchorKey) else { return nil }
    return try? JSONDecoder().decode(PanelAnchor.self, from: data)
  }

  private func saveAnchor(_ anchor: PanelAnchor) {
    guard let data = try? JSONEncoder().encode(anchor) else { return }
    defaults.set(data, forKey: Self.anchorKey)
  }
}

/// Owns the panel and its model for the app's lifetime, follows
/// `controller.detection.prompt` and `controller.recorder.recording` through
/// `withObservationTracking`, and resolves what to show with
/// `FloatingContent.resolve`. The tracking is one-shot and `onChange` runs on
/// whichever thread performed the mutation, so every change hops back to the
/// main actor and re-registers. The panel is created in `follow`, after
/// launch, and ordered in on the first non-hidden content.
@MainActor
final class FloatingPanelPresenter {
  private let defaults: UserDefaults
  private var panel: FloatingPanel?
  private var model: FloatingPanelModel?
  private var controller: AppController?
  private var observers: [any NSObjectProtocol] = []

  init(defaults: UserDefaults = .standard) {
    self.defaults = defaults
  }

  /// Starts following the controller. `openMain` opens the main window; it
  /// is captured inside a SwiftUI scene because a view hosted in this
  /// panel is outside every scene and its `openWindow` does nothing.
  func follow(_ controller: AppController, openMain: @escaping @MainActor () -> Void) {
    guard self.controller == nil else { return }
    self.controller = controller
    let panel = FloatingPanel()
    let model = FloatingPanelModel(host: panel, defaults: defaults)
    self.panel = panel
    self.model = model
    panel.setContent(
      AnyView(FloatingPanelRoot(state: model.state, controller: controller, openMain: openMain)))
    let center = NotificationCenter.default
    observers.append(
      center.addObserver(forName: NSWindow.didMoveNotification, object: panel, queue: .main) {
        [weak self] _ in
        Task { @MainActor [weak self] in self?.panelDidMove() }
      })
    observers.append(
      center.addObserver(forName: NSWindow.didResizeNotification, object: panel, queue: .main) {
        [weak self] _ in
        Task { @MainActor [weak self] in self?.panelDidResize() }
      })
    observers.append(
      center.addObserver(
        forName: NSApplication.didChangeScreenParametersNotification, object: nil, queue: .main
      ) { [weak self] _ in
        Task { @MainActor [weak self] in self?.model?.screensDidChange() }
      })
    observe()
  }

  private func observe() {
    guard let controller, let model else { return }
    let content = withObservationTracking {
      FloatingContent.resolve(
        prompt: controller.detection.prompt, recording: controller.recorder.recording)
    } onChange: { [weak self] in
      Task { @MainActor [weak self] in self?.observe() }
    }
    model.apply(content)
  }

  private func panelDidMove() {
    guard let panel, let model else { return }
    model.panelDidMove(to: panel.frame)
  }

  private func panelDidResize() {
    guard let panel, let model else { return }
    model.panelDidResize(to: panel.frame.size)
  }
}

/// The panel's SwiftUI root: the prompt or the bubble, crossfading over
/// `Motion.functional` (no animation under Reduce Motion) while the window
/// follows the content's size.
struct FloatingPanelRoot: View {
  let state: FloatingPanelState
  let controller: AppController
  let openMain: @MainActor () -> Void
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  var body: some View {
    ZStack {
      switch state.content {
      case .prompt(let model):
        DetectionPromptView(model: model)
          .id(model.id)
          .transition(.opacity)
      case .bubble:
        RecordingBubbleView(controller: controller, openMain: openMain)
          .transition(.opacity)
      case nil:
        EmptyView()
      }
    }
    .animation(Motion.swap(reduceMotion: reduceMotion), value: state.content)
  }
}
