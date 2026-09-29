import Foundation
import StenoAudio

// The pure pieces of the floating panel: what it shows, how the bubble and
// the menu bar label render a recorder state, where the panel sits, and the
// five-sample level history. None of this touches AppKit, so every rule
// here has a hostless test.

/// What the one floating panel shows: the detection prompt, the recording
/// bubble, or nothing.
enum FloatingContent: Equatable {
  case prompt(DetectionPromptViewModel)
  case bubble

  /// The one rule: any recorder state other than `.idle` wins, else a prompt
  /// if one exists, else hidden. The surfaces drive the recorder and never
  /// each other.
  static func resolve(prompt: DetectionPromptViewModel?, recording: RecordingState)
    -> FloatingContent?
  {
    switch recording {
    case .starting, .recording, .stopping:
      return .bubble
    case .idle:
      return prompt.map { .prompt($0) }
    }
  }

  static func == (lhs: FloatingContent, rhs: FloatingContent) -> Bool {
    switch (lhs, rhs) {
    case (.bubble, .bubble): true
    case (.prompt(let left), .prompt(let right)): left === right
    default: false
    }
  }
}

/// What the bubble renders for a recorder state: the transient text (with
/// a spinner beside it), the bars, the stop button and its enabled state,
/// and the auto-stop row (`AutoStopPresentation`, the recorder's value; see
/// `Recording/AutoStop.swift`).
struct BubblePresentation: Equatable, Sendable {
  /// "Starting…" or "Finishing…"; nil while recording (the clock shows).
  var text: String?
  var since: Date?
  var showsBars: Bool
  var showsStop: Bool
  var stopEnabled: Bool
  var autoStop: AutoStopPresentation?

  static func make(state: RecordingState, autoStop: AutoStopPresentation?) -> BubblePresentation {
    switch state {
    case .idle:
      return BubblePresentation(
        text: nil, since: nil, showsBars: false, showsStop: false, stopEnabled: false,
        autoStop: nil)
    case .starting:
      return BubblePresentation(
        text: state.label, since: nil, showsBars: false, showsStop: false, stopEnabled: false,
        autoStop: nil)
    case .recording(let since):
      return BubblePresentation(
        text: nil, since: since, showsBars: true, showsStop: true, stopEnabled: true,
        autoStop: autoStop)
    case .stopping:
      return BubblePresentation(
        text: state.label, since: nil, showsBars: false, showsStop: true, stopEnabled: false,
        autoStop: nil)
    }
  }
}

/// The menu bar item's label: `waveform` when idle, `record.circle.fill`
/// while the recorder is busy, the symbol plus the elapsed time while
/// recording, so the state is legible at a glance and survives template
/// rendering of the image.
struct MenuBarLabelPresentation: Equatable, Sendable {
  var symbolName: String
  var elapsedText: String?
  /// "Steno", "Steno, recording", or "Steno, recording, 12:34".
  var accessibilityLabel: String

  static func make(state: RecordingState, now: Date) -> MenuBarLabelPresentation {
    switch state {
    case .idle:
      return MenuBarLabelPresentation(
        symbolName: "waveform", elapsedText: nil, accessibilityLabel: "Steno")
    case .starting, .stopping:
      return MenuBarLabelPresentation(
        symbolName: "record.circle.fill", elapsedText: nil, accessibilityLabel: "Steno, recording")
    case .recording(let since):
      let elapsed = now.timeIntervalSince(since).clockText
      return MenuBarLabelPresentation(
        symbolName: "record.circle.fill", elapsedText: elapsed,
        accessibilityLabel: "Steno, recording, \(elapsed)")
    }
  }
}

/// Where the panel sits: the top-centre point of its frame, in screen
/// coordinates, with the visible frame of the screen it was saved on. Both
/// contents are laid out from this one point, so the prompt morphs into the
/// bubble without moving.
struct PanelAnchor: Codable, Equatable, Sendable {
  var topCenter: CGPoint
  var screenFrame: CGRect

  /// The default anchor on `visibleFrame`: its top centre, `Theme.Space.sm`
  /// under the menu bar.
  static func `default`(in visibleFrame: CGRect) -> PanelAnchor {
    PanelAnchor(
      topCenter: CGPoint(x: visibleFrame.midX, y: visibleFrame.maxY - Theme.Space.sm),
      screenFrame: visibleFrame)
  }

  /// The frame of a panel of `size` whose top-centre point is the anchor.
  func frame(for size: CGSize) -> CGRect {
    CGRect(
      x: (topCenter.x - size.width / 2).rounded(), y: (topCenter.y - size.height).rounded(),
      width: size.width, height: size.height)
  }

  /// The anchor that describes a panel with `frame`, on the screen among
  /// `screens` that holds its top-centre point (else `fallback`).
  static func from(frame: CGRect, screens: [CGRect], fallback: CGRect) -> PanelAnchor {
    let point = CGPoint(x: frame.midX, y: frame.maxY)
    let screen = screens.first { $0.contains(point) } ?? fallback
    return PanelAnchor(topCenter: point, screenFrame: screen)
  }

  /// A saved anchor is kept while a panel of `size` hanging from it lies
  /// within one of the current screens' visible frames (the point alone
  /// would accept a panel below the screen's bottom edge); otherwise the
  /// default anchor on `fallback`.
  static func validated(_ saved: PanelAnchor?, size: CGSize, screens: [CGRect], fallback: CGRect)
    -> PanelAnchor
  {
    if let saved, screens.contains(where: { $0.contains(saved.frame(for: size)) }) {
      return saved
    }
    return .default(in: fallback)
  }
}

/// The last five level samples as bar fractions, newest last. Silence and
/// a missing sample map to 0; `system` nil uses `mic` alone.
struct LiveBarsHistory: Equatable, Sendable {
  static let count = 5

  private(set) var fractions: [CGFloat] = Array(repeating: 0, count: LiveBarsHistory.count)

  mutating func push(levels: LaneLevels?) {
    let fraction: CGFloat
    if let levels {
      let loudest = max(levels.mic.rms, levels.system?.rms ?? -Float.infinity)
      fraction = LevelBars.fraction(loudest)
    } else {
      fraction = 0
    }
    fractions.removeFirst()
    fractions.append(fraction)
  }
}
