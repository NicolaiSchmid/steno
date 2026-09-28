import StenoAudio
import SwiftUI

// The recording bubble and the pieces it shares with the detection prompt:
// the bar background, the hover veil, the glyph, the five live bars and the
// countdown hairline. One hue on every surface: the stop glyph is
// `destructive`; the bars are achromatic.

/// The geometry of the floating panel's contents.
enum PanelMetrics {
  static let bubbleHeight: CGFloat = 40
  static let bubbleInset: CGFloat = 6
  static let bubbleGap: CGFloat = 10
  static let promptHeight: CGFloat = 56
  static let promptMinWidth: CGFloat = 360
  static let promptMaxWidth: CGFloat = 480
  static let glyphWell: CGFloat = 20
  static let glyphSize: CGFloat = 16
  static let stopSize: CGFloat = 28
  static let stopGlyphSize: CGFloat = 11
  static let barWidth: CGFloat = 3
  static let barGap: CGFloat = 2
  static let barMinHeight: CGFloat = 4
  static let barMaxHeight: CGFloat = 16
  static let dismissSize: CGFloat = 24
  static let dismissHitSize: CGFloat = 28
  static let dismissGlyphSize: CGFloat = 11
  static let hairlineHeight: CGFloat = 2
  static let hairlineInset: CGFloat = 12
  static let hairlineBottom: CGFloat = 4
  static let spinnerSize: CGFloat = 12
  static let hairlineFillOpacity: Double = 0.4
}

/// The panel's bar: `popover` fill (opaque in both appearances), hairline
/// `border`, radius `xl`; a `card` veil while hovered when the caller says so.
struct PanelBar: ViewModifier {
  var hovering = false

  func body(content: Content) -> some View {
    content
      .background(
        ZStack {
          Theme.Radius.xl.shape.fill(Color.stenoPopover)
          Theme.Radius.xl.shape.fill(hovering ? Color.stenoCard : Color.clear)
        }
      )
      .overlay(Theme.Radius.xl.shape.hairline())
      .contentShape(Theme.Radius.xl.shape)
  }
}

/// The app glyph: `waveform` in a 20 pt `card` well until the icon plan
/// ships `NSApp.applicationIconImage`; the switch is this one view.
struct BubbleGlyph: View {
  static let symbolName = "waveform"

  var body: some View {
    Image(systemName: Self.symbolName)
      .font(.system(size: PanelMetrics.glyphSize, weight: .semibold))
      .foregroundStyle(Color.stenoStrong)
      .frame(width: PanelMetrics.glyphWell, height: PanelMetrics.glyphWell)
      .background(Theme.Radius.sm.shape.fill(Color.stenoCard))
      .accessibilityHidden(true)
  }
}

/// Five 3 pt bars, `strong` over a `border` track, heights 4 to 16 from the
/// last five level samples, newest trailing. Heights animate over
/// `Motion.functional` and step under Reduce Motion.
struct LiveBars: View {
  let history: LiveBarsHistory
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  var body: some View {
    HStack(alignment: .center, spacing: PanelMetrics.barGap) {
      ForEach(Array(history.fractions.enumerated()), id: \.offset) { _, fraction in
        ZStack {
          Capsule().fill(Color.stenoBorder)
          Capsule()
            .fill(Color.stenoStrong)
            .frame(height: Self.height(for: fraction))
        }
        .frame(width: PanelMetrics.barWidth, height: PanelMetrics.barMaxHeight)
      }
    }
    .animation(Motion.swap(reduceMotion: reduceMotion), value: history)
    .accessibilityHidden(true)
  }

  static func height(for fraction: CGFloat) -> CGFloat {
    PanelMetrics.barMinHeight + (PanelMetrics.barMaxHeight - PanelMetrics.barMinHeight) * fraction
  }
}

/// The draining countdown: a 2 pt `border` track with a `strong` fill at 40
/// percent whose width is the fraction remaining. The prompt shows it
/// without a number; the auto-stop shows both.
struct CountdownHairline: View {
  let countdown: Countdown
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  var body: some View {
    GeometryReader { proxy in
      ZStack(alignment: .leading) {
        Capsule().fill(Color.stenoBorder)
        Capsule()
          .fill(Color.stenoStrong.opacity(PanelMetrics.hairlineFillOpacity))
          .frame(width: proxy.size.width * countdown.fractionRemaining)
          .animation(Motion.countdown(reduceMotion: reduceMotion), value: countdown.remaining)
      }
    }
    .frame(height: PanelMetrics.hairlineHeight)
    .accessibilityHidden(true)
  }
}

/// The 28 pt stop square: `raised`, hairline, a `destructive` `stop.fill`
/// glyph that brightens to `destructiveStrong` on the `card` hover veil;
/// dimmed to half while disabled.
struct BubbleStopButton: View {
  let isEnabled: Bool
  let action: () -> Void
  @State private var hovering = false
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  var body: some View {
    Button(action: action) {
      Image(systemName: "stop.fill")
        .font(.system(size: PanelMetrics.stopGlyphSize, weight: .semibold))
        .foregroundStyle(
          hovering && isEnabled ? Color.stenoDestructiveStrong : Color.stenoDestructive
        )
        .frame(width: PanelMetrics.stopSize, height: PanelMetrics.stopSize)
        .background(
          ZStack {
            Theme.Radius.md.shape.fill(Color.stenoRaised)
            Theme.Radius.md.shape.fill(hovering && isEnabled ? Color.stenoCard : Color.clear)
          }
        )
        .overlay(Theme.Radius.md.shape.hairline())
        .contentShape(Theme.Radius.md.shape)
    }
    .buttonStyle(.plain)
    .disabled(!isEnabled)
    .opacity(isEnabled ? 1 : Motion.disabledOpacity)
    .onHover { hovering = $0 }
    .animation(Motion.swap(reduceMotion: reduceMotion), value: hovering)
    .help("Stop recording")
    .accessibilityLabel("Stop recording")
    .accessibilityIdentifier("bubble-stop")
  }
}

/// The bubble over the one recorder: shown for every state but `.idle`.
/// The body opens the live meeting in the main window; the square stops
/// the recording through the shared `recorder.stop()`.
struct RecordingBubbleView: View {
  let controller: AppController
  let openMain: @MainActor () -> Void
  @State private var history = LiveBarsHistory()

  private var recorder: RecordingController { controller.recorder }

  var body: some View {
    BubbleBody(
      presentation: BubblePresentation.make(state: recorder.recording, autoStop: nil),
      history: history,
      elapsedValue: recorder.elapsed?.clockText,
      open: { open() },
      stop: { Task { await recorder.stop() } }
    )
    .onChange(of: recorder.levels) { _, levels in
      history.push(levels: levels)
    }
    .onChange(of: recorder.recording) { _, recording in
      if case .recording = recording { return }
      history = LiveBarsHistory()
    }
  }

  private func open() {
    if let live = recorder.activeMeetingID {
      controller.requestedMeetingID = live
    }
    openMain()
  }
}

/// The bubble's layout for one presentation, with no recorder behind it,
/// so the previews render every state.
struct BubbleBody: View {
  let presentation: BubblePresentation
  let history: LiveBarsHistory
  /// The elapsed time as VoiceOver reads it.
  let elapsedValue: String?
  let open: () -> Void
  let stop: () -> Void
  @State private var hovering = false
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  var body: some View {
    HStack(spacing: PanelMetrics.bubbleGap) {
      Button(action: open) {
        HStack(spacing: PanelMetrics.bubbleGap) {
          BubbleGlyph()
          middle
        }
        .contentShape(Rectangle())
      }
      .buttonStyle(.plain)
      .accessibilityLabel(presentation.text ?? "Recording")
      .accessibilityValue(elapsedValue ?? "")
      .accessibilityHint("Opens the meeting in Steno")
      .accessibilityIdentifier("bubble-open")
      if presentation.showsStop {
        BubbleStopButton(isEnabled: presentation.stopEnabled, action: stop)
      }
    }
    .padding(PanelMetrics.bubbleInset)
    .frame(height: PanelMetrics.bubbleHeight)
    .modifier(PanelBar(hovering: hovering))
    .onHover { hovering = $0 }
    .animation(Motion.swap(reduceMotion: reduceMotion), value: hovering)
    .accessibilityElement(children: .contain)
    .accessibilityIdentifier("recording-bubble")
  }

  @ViewBuilder
  private var middle: some View {
    if let text = presentation.text {
      Text(text)
        .font(.steno(Theme.TextSize.xxs))
        .foregroundStyle(Color.stenoMutedForeground)
      ProgressView()
        .controlSize(.mini)
        .frame(width: PanelMetrics.spinnerSize, height: PanelMetrics.spinnerSize)
    } else {
      if presentation.showsBars {
        LiveBars(history: history)
      }
      if let since = presentation.since {
        ElapsedText(since: since)
          .font(.steno(Theme.TextSize.xxs))
          .foregroundStyle(Color.stenoMutedForeground)
      }
    }
  }
}

#if DEBUG
  private struct BubblePreviews: View {
    var sampleHistory: LiveBarsHistory {
      var history = LiveBarsHistory()
      for rms in [-40, -18, -9, -24, -12] as [Float] {
        history.push(levels: LaneLevels(mic: LaneLevel(rms: rms, peak: rms), system: nil))
      }
      return history
    }

    var body: some View {
      VStack(alignment: .leading, spacing: Theme.Space.lg) {
        BubbleBody(
          presentation: .make(state: .starting, autoStop: nil), history: LiveBarsHistory(),
          elapsedValue: nil, open: {}, stop: {})
        BubbleBody(
          presentation: .make(
            state: .recording(since: .now.addingTimeInterval(-754)), autoStop: nil),
          history: sampleHistory, elapsedValue: "12:34", open: {}, stop: {})
        BubbleBody(
          presentation: .make(state: .stopping, autoStop: nil), history: LiveBarsHistory(),
          elapsedValue: nil, open: {}, stop: {})
      }
    }
  }

  #Preview("Recording bubble") {
    PreviewPair { BubblePreviews() }
      .frame(width: 560, height: 260)
  }
#endif
