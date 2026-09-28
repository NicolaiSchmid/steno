import AppKit
import Foundation
import Observation
import SwiftUI

/// The panel's rules over a `PanelHost`: content resolution, anchor load,
/// save and re-validation, and the observation-to-apply sequence. Pure over
/// the host, so anchor persistence and screen changes are unit tests
/// against a fake. `content` is what the SwiftUI root renders; the root
/// observes it and crossfades between the prompt and the bubble. The
/// window's size comes from that root through `contentSizeDidChange`,
/// never from AppKit layout: the plan's `NSHostingController` with
/// `preferredContentSize` re-entered layout and hung the main thread for
/// 30 s on the hosted runner once the bubble appeared.
@MainActor
@Observable
final class FloatingPanelModel {
  static let anchorKey = "steno.floatingPanel.anchor"

  /// Kept while the panel fades out, so the exit is opacity only.
  private(set) var content: FloatingContent?
  private let host: any PanelHost
  private let defaults: UserDefaults
  private(set) var anchor: PanelAnchor?
  /// The content's last reported size; nil until the root has measured.
  private(set) var lastSize: CGSize?
  /// True between `apply(content)` and `apply(nil)`.
  private(set) var wantsShown = false

  init(host: any PanelHost, defaults: UserDefaults) {
    self.host = host
    self.defaults = defaults
  }

  private var fallbackScreen: CGRect { host.currentScreens.first ?? .zero }

  /// The size the anchor is validated against before the root has measured:
  /// a one-point-wide bubble row, so the vertical extent is what counts
  /// (a zero-width rectangle is empty and `contains` would accept it
  /// anywhere).
  private var validationSize: CGSize {
    lastSize ?? CGSize(width: 1, height: PanelMetrics.bubbleHeight)
  }

  /// The anchor to lay out from: the saved one while a panel there lies on
  /// a current screen, else the default on the main screen.
  func currentAnchor() -> PanelAnchor {
    let resolved = PanelAnchor.validated(
      anchor ?? loadAnchor(), size: validationSize, screens: host.currentScreens,
      fallback: fallbackScreen)
    anchor = resolved
    return resolved
  }

  /// Nil hides the panel. Content shows it at the anchor once its size is
  /// known: at once when the same content was measured before, otherwise
  /// when the root reports the new content's size, so the window never
  /// sits at the previous content's frame (a content change forgets the
  /// last size; the recorder's `.starting` to `.recording` step arrives
  /// before the bubble has measured).
  func apply(_ content: FloatingContent?) {
    guard let content else {
      wantsShown = false
      host.hide()
      return
    }
    wantsShown = true
    if self.content != content {
      self.content = content
      lastSize = nil
      return
    }
    if let lastSize { place(size: lastSize) }
  }

  /// The root measured its content: the panel takes that size at the
  /// held top-centre point. Sub-point differences are AppKit rounding.
  func contentSizeDidChange(_ size: CGSize) {
    guard size.width > 0, size.height > 0 else { return }
    if let lastSize, Self.matches(size, lastSize) { return }
    lastSize = size
    if wantsShown { place(size: size) }
  }

  /// The user dragged the panel: the anchor follows and is saved. A move
  /// that arrives with a size other than the one last placed is the window
  /// still taking its content's size and is not a drag.
  func panelDidMove(to frame: CGRect) {
    guard let lastSize, Self.matches(frame.size, lastSize) else { return }
    let moved = PanelAnchor.from(
      frame: frame, screens: host.currentScreens, fallback: fallbackScreen)
    guard moved != anchor else { return }
    anchor = moved
    saveAnchor(moved)
  }

  /// A display appeared or disappeared: a saved anchor on a screen that is
  /// gone falls back to the default anchor on the remaining screen, and the
  /// stale anchor is replaced in the defaults.
  func screensDidChange() {
    let before = anchor
    let resolved = currentAnchor()
    if let before, resolved != before { saveAnchor(resolved) }
    guard wantsShown, let lastSize else { return }
    host.show(frame: resolved.frame(for: lastSize))
  }

  /// AppKit may round a frame to the pixel grid; a difference under a point
  /// is the same size.
  private static func matches(_ lhs: CGSize, _ rhs: CGSize) -> Bool {
    abs(lhs.width - rhs.width) < 1 && abs(lhs.height - rhs.height) < 1
  }

  private func place(size: CGSize) {
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
/// main actor and re-registers. The same observation drives the shared
/// `RecordingClock`. The host is created in `follow`, after launch, and
/// ordered in on the first non-hidden content; the tests inject a fake.
@MainActor
final class FloatingPanelPresenter {
  private let defaults: UserDefaults
  private let makeHost: @MainActor () -> any PanelHost
  private var host: (any PanelHost)?
  private(set) var model: FloatingPanelModel?
  private var controller: AppController?
  private var clock: RecordingClock?
  private var observers: [any NSObjectProtocol] = []

  init(
    defaults: UserDefaults = .standard,
    makeHost: @escaping @MainActor () -> any PanelHost = { FloatingPanel() }
  ) {
    self.defaults = defaults
    self.makeHost = makeHost
  }

  /// Starts following the controller. `openMain` opens the main window; it
  /// is captured inside a SwiftUI scene because a view hosted in this
  /// panel is outside every scene and its `openWindow` does nothing.
  func follow(
    _ controller: AppController, clock: RecordingClock,
    openMain: @escaping @MainActor () -> Void
  ) {
    guard self.controller == nil else { return }
    self.controller = controller
    self.clock = clock
    let host = makeHost()
    let model = FloatingPanelModel(host: host, defaults: defaults)
    self.host = host
    self.model = model
    host.setContent(
      AnyView(
        FloatingPanelRoot(
          model: model, controller: controller, clock: clock, openMain: openMain,
          onSize: { [weak model] size in
            // Off the layout pass: the window frame changes on the next turn.
            Task { @MainActor [weak model] in model?.contentSizeDidChange(size) }
          })))
    let center = NotificationCenter.default
    if let window = host as? NSWindow {
      observers.append(
        center.addObserver(forName: NSWindow.didMoveNotification, object: window, queue: .main) {
          [weak self] _ in
          Task { @MainActor [weak self] in self?.panelDidMove() }
        })
    }
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
    let recording = controller.recorder.recording
    let content = withObservationTracking {
      FloatingContent.resolve(
        prompt: controller.detection.prompt, recording: controller.recorder.recording)
    } onChange: { [weak self] in
      Task { @MainActor [weak self] in self?.observe() }
    }
    clock?.update(for: recording)
    model.apply(content)
  }

  private func panelDidMove() {
    guard let window = host as? NSWindow, let model else { return }
    model.panelDidMove(to: window.frame)
  }
}

/// The panel's SwiftUI root: the prompt or the bubble at their intrinsic
/// size, crossfading over `Motion.functional` (no animation under Reduce
/// Motion), pinned to the window's top-leading corner. It reports its size
/// through `onSize`, which is how the window gets its frame.
struct FloatingPanelRoot: View {
  let model: FloatingPanelModel
  let controller: AppController
  let clock: RecordingClock
  let openMain: @MainActor () -> Void
  let onSize: (CGSize) -> Void
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  var body: some View {
    ZStack {
      switch model.content {
      case .prompt(let prompt):
        DetectionPromptView(model: prompt)
          .id(prompt.id)
          .transition(.opacity)
      case .bubble:
        RecordingBubbleView(controller: controller, clock: clock, openMain: openMain)
          .transition(.opacity)
      case nil:
        EmptyView()
      }
    }
    .animation(Motion.swap(reduceMotion: reduceMotion), value: model.content)
    .fixedSize()
    .onGeometryChange(for: CGSize.self) { proxy in
      proxy.size
    } action: { size in
      onSize(size)
    }
    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
  }
}
