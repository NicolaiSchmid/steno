import AppKit
import SwiftUI

/// What the panel model drives: one window that shows a frame, hides, and
/// reports the screens and its content's size. `FloatingPanel` is the
/// product conformance; the tests use a fake.
@MainActor
protocol PanelHost: AnyObject {
  /// The visible frames of the current screens, the main screen first.
  var currentScreens: [CGRect] { get }
  /// The size the SwiftUI content wants right now.
  var contentFittingSize: CGSize { get }
  var isShown: Bool { get }
  func setContent(_ view: AnyView)
  func show(frame: CGRect)
  func hide()
}

/// The one borderless, non-activating, always-on-top panel that hosts the
/// detection prompt and the recording bubble. It never becomes key or main
/// and never activates Steno; the user drags it by its background. The
/// window shadow is the one shadow the "hairlines, not shadows" rule does
/// not cover: a floating panel over arbitrary content needs separation.
/// Content comes through an `NSHostingController` sized by
/// `preferredContentSize`, so the window follows the SwiftUI size; the
/// presenter re-applies the anchor after every resize.
@MainActor
final class FloatingPanel: NSPanel, PanelHost {
  private var hosting: NSHostingController<AnyView>?
  /// True from `show` until `hide` finishes, so a hide that is still fading
  /// does not order out a panel shown again meanwhile.
  private(set) var isShown = false
  private var hideGeneration = 0

  init() {
    super.init(
      contentRect: NSRect(x: 0, y: 0, width: 200, height: Theme.Control.ctaHeight),
      styleMask: [.nonactivatingPanel, .borderless], backing: .buffered, defer: false)
    level = .floating
    collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary]
    hidesOnDeactivate = false
    isMovableByWindowBackground = true
    isReleasedWhenClosed = false
    becomesKeyOnlyIfNeeded = true
    isOpaque = false
    backgroundColor = .clear
    hasShadow = true
    animationBehavior = .none
    isExcludedFromWindowsMenu = true
  }

  override var canBecomeKey: Bool { false }
  override var canBecomeMain: Bool { false }

  // MARK: - PanelHost

  var currentScreens: [CGRect] {
    var frames = NSScreen.screens.map(\.visibleFrame)
    if let main = NSScreen.main?.visibleFrame, let index = frames.firstIndex(of: main) {
      frames.remove(at: index)
      frames.insert(main, at: 0)
    }
    return frames
  }

  var contentFittingSize: CGSize {
    guard let hosting else { return frame.size }
    hosting.view.layoutSubtreeIfNeeded()
    let preferred = hosting.preferredContentSize
    if preferred.width > 0, preferred.height > 0 { return preferred }
    return hosting.view.fittingSize
  }

  func setContent(_ view: AnyView) {
    if let hosting {
      hosting.rootView = view
      return
    }
    let hosting = NSHostingController(rootView: view)
    hosting.sizingOptions = .preferredContentSize
    self.hosting = hosting
    contentViewController = hosting
  }

  /// Orders the panel front at `frame`, fading in over `Motion.entrance`
  /// when it was hidden; moves it without animation when it is shown.
  func show(frame: CGRect) {
    hideGeneration += 1
    if isShown, isVisible {
      if self.frame != frame { setFrame(frame, display: true) }
      return
    }
    isShown = true
    alphaValue = 0
    setFrame(frame, display: false)
    orderFrontRegardless()
    NSAnimationContext.runAnimationGroup { context in
      context.duration = Motion.durationEntrance
      context.timingFunction = CAMediaTimingFunction(controlPoints: 0.4, 0, 0.2, 1)
      animator().alphaValue = 1
    }
  }

  /// Fades out over `Motion.exit`, then orders out, unless shown again.
  func hide() {
    guard isShown else { return }
    isShown = false
    hideGeneration += 1
    let generation = hideGeneration
    NSAnimationContext.runAnimationGroup(
      { context in
        context.duration = Motion.durationExit
        context.timingFunction = CAMediaTimingFunction(controlPoints: 0.4, 0, 0.2, 1)
        animator().alphaValue = 0
      },
      completionHandler: { [weak self] in
        Task { @MainActor [weak self] in
          guard let self, self.hideGeneration == generation, !self.isShown else { return }
          self.orderOut(nil)
        }
      })
  }
}
