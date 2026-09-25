import AppKit
import SwiftUI

/// A floating, non-activating `NSPanel` in the top-right corner of the main
/// screen hosting `DetectionPromptView`. `DetectionPanelPresenter` shows one
/// panel per prompt and closes it when the prompt closes.
@MainActor
final class DetectionPanelPresenter {
  private var panel: NSPanel?
  private var shownPromptID: UUID?

  init() {}

  func present(_ prompt: DetectionPromptViewModel?) {
    guard let prompt else {
      dismiss()
      return
    }
    guard prompt.id != shownPromptID else { return }
    dismiss()
    let content = NSHostingView(rootView: DetectionPromptView(model: prompt))
    let size = NSSize(width: 360, height: 132)
    let panel = NSPanel(
      contentRect: NSRect(origin: .zero, size: size),
      styleMask: [.nonactivatingPanel, .titled, .fullSizeContentView, .utilityWindow],
      backing: .buffered, defer: false)
    panel.titleVisibility = .hidden
    panel.titlebarAppearsTransparent = true
    panel.isMovableByWindowBackground = true
    panel.level = .floating
    panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
    panel.isReleasedWhenClosed = false
    panel.hidesOnDeactivate = false
    panel.backgroundColor = Theme.popover.nsColor
    panel.contentView = content
    if let screen = NSScreen.main {
      let frame = screen.visibleFrame
      let origin = NSPoint(
        x: frame.maxX - size.width - Theme.Space.lg, y: frame.maxY - size.height - Theme.Space.lg)
      panel.setFrameOrigin(origin)
    }
    panel.orderFrontRegardless()
    self.panel = panel
    self.shownPromptID = prompt.id
  }

  func dismiss() {
    panel?.orderOut(nil)
    panel = nil
    shownPromptID = nil
  }
}

struct DetectionPromptView: View {
  let model: DetectionPromptViewModel

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.md) {
      HStack(spacing: Theme.Space.sm) {
        Circle().fill(Color.stenoLiveBright).frame(width: 8, height: 8)
        Text("\(model.appName) opened the microphone")
          .font(.steno(Theme.TextSize.sm, weight: .semibold))
          .foregroundStyle(Color.stenoStrong)
        Spacer()
        Text("\(model.remainingSeconds)s")
          .font(.steno(Theme.TextSize.xxs).monospacedDigit())
          .foregroundStyle(Color.stenoFaint)
      }
      Text("Record this call with Steno? Audio stays on this Mac.")
        .font(.steno(Theme.TextSize.xs))
        .foregroundStyle(Color.stenoMutedForeground)
      HStack(spacing: Theme.Space.sm) {
        Spacer()
        Button("Not now") { Task { await model.dismiss() } }
          .buttonStyle(StenoSecondaryButtonStyle())
          .keyboardShortcut(.cancelAction)
        Button("Record") { Task { await model.start() } }
          .buttonStyle(StenoPrimaryButtonStyle())
          .keyboardShortcut(.defaultAction)
      }
    }
    .padding(Theme.Space.lg)
    .frame(width: 360)
    .background(Color.stenoPopover)
  }
}
