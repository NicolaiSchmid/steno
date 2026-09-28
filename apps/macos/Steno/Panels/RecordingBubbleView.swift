import StenoAudio
import SwiftUI

// The recording bubble and the pieces it shares with the detection prompt:
// the bar background, the hover veil, the glyph, the five live bars and the
// countdown hairline. One hue on every surface: the stop glyph is
// `destructive`; the bars are achromatic.

/// The geometry of the floating panel's contents.
enum PanelMetrics {
  static let bubbleHeight: CGFloat = 40
  /// The bubble with the armed auto-stop row under the controls.
  static let bubbleArmedHeight: CGFloat = 64
  static let bubbleInset: CGFloat = 6
  static let bubbleGap: CGFloat = 10
  static let bubbleMaxWidth: CGFloat = 480
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
/// percent whose width is the fraction remaining, stepping over
/// `Motion.countdown` (no animation under Reduce Motion). The prompt shows
/// it without a number; the auto-stop shows both. Takes the fraction, so
/// the caller decides what it observes (the `Countdown` class or the
/// recorder's `AutoStopPresentation` value).
struct CountdownHairline: View {
  let fractionRemaining: Double
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  var body: some View {
    GeometryReader { proxy in
      ZStack(alignment: .leading) {
        Capsule().fill(Color.stenoBorder)
        Capsule()
          .fill(Color.stenoStrong.opacity(PanelMetrics.hairlineFillOpacity))
          .frame(width: proxy.size.width * min(1, max(0, fractionRemaining)))
          .animation(Motion.countdown(reduceMotion: reduceMotion), value: fractionRemaining)
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

/// A 28 pt text button for the bubble: `sm` `strong` text in a radius 8
/// box, the `card` veil on hover. "Keep recording" sits in one, before
/// the stop square, so the two choices carry equal weight.
struct BubbleTextButton: View {
  let title: String
  let identifier: String
  let action: () -> Void
  @State private var hovering = false
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  var body: some View {
    Button(action: action) {
      Text(title)
        .font(.steno(Theme.TextSize.sm, weight: .medium))
        .foregroundStyle(Color.stenoStrong)
        .padding(.horizontal, Theme.Control.rowInset)
        .frame(height: PanelMetrics.stopSize)
        .background(Theme.Radius.md.shape.fill(hovering ? Color.stenoCard : Color.clear))
        .contentShape(Theme.Radius.md.shape)
    }
    .buttonStyle(.plain)
    .onHover { hovering = $0 }
    .animation(Motion.swap(reduceMotion: reduceMotion), value: hovering)
    .accessibilityIdentifier(identifier)
  }
}

/// The bubble over the one recorder: shown for every state but `.idle`.
/// The body opens the live meeting in the main window; the square stops
/// the recording through the shared `recorder.stop()`; while the auto-stop
/// is armed, "Keep recording" calls `recorder.keepRecording()`.
struct RecordingBubbleView: View {
  let controller: AppController
  /// The shared 1 Hz clock; the elapsed time is `clock.now` minus `since`.
  let clock: RecordingClock
  let openMain: @MainActor () -> Void
  @State private var history = LiveBarsHistory()

  private var recorder: RecordingController { controller.recorder }

  var body: some View {
    let presentation = BubblePresentation.make(
      state: recorder.recording, autoStop: recorder.autoStop?.presentation)
    BubbleBody(
      presentation: presentation,
      history: history,
      elapsedText: presentation.since.map { clock.elapsed(since: $0).clockText },
      open: { open() },
      keepRecording: { recorder.keepRecording() },
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
/// so the previews render every state. One row of 40 pt; with the auto-stop
/// armed, 64 pt: the countdown line under the controls and the hairline
/// along the bottom.
struct BubbleBody: View {
  let presentation: BubblePresentation
  let history: LiveBarsHistory
  /// The elapsed time, `mm:ss`; shown while recording and read by VoiceOver.
  let elapsedText: String?
  let open: () -> Void
  let keepRecording: () -> Void
  let stop: () -> Void
  @State private var hovering = false
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  private var height: CGFloat {
    presentation.autoStop == nil ? PanelMetrics.bubbleHeight : PanelMetrics.bubbleArmedHeight
  }

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.xs) {
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
        .accessibilityValue(elapsedText ?? "")
        .accessibilityHint("Opens the meeting in Steno")
        .accessibilityIdentifier("bubble-open")
        if presentation.autoStop != nil {
          Spacer(minLength: 0)
          BubbleTextButton(
            title: AutoStopPresentation.keepRecordingLabel, identifier: "bubble-keep-recording",
            action: keepRecording)
        }
        if presentation.showsStop {
          BubbleStopButton(isEnabled: presentation.stopEnabled, action: stop)
        }
      }
      if let autoStop = presentation.autoStop {
        Text(autoStop.line)
          .font(.steno(Theme.TextSize.sm))
          .foregroundStyle(Color.stenoStrong)
          .lineLimit(1)
          .padding(.horizontal, Theme.Space.xs)
          .accessibilityIdentifier("bubble-auto-stop")
      }
    }
    .padding(PanelMetrics.bubbleInset)
    .frame(height: height)
    .frame(maxWidth: PanelMetrics.bubbleMaxWidth)
    .overlay(alignment: .bottom) {
      if let autoStop = presentation.autoStop {
        CountdownHairline(fractionRemaining: autoStop.fractionRemaining)
          .padding(.horizontal, PanelMetrics.hairlineInset)
          .padding(.bottom, PanelMetrics.hairlineBottom)
      }
    }
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
      if let elapsedText {
        Text(elapsedText)
          .font(.steno(Theme.TextSize.xxs))
          .monospacedDigit()
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
      let since = Date.now.addingTimeInterval(-754)
      VStack(alignment: .leading, spacing: Theme.Space.lg) {
        BubbleBody(
          presentation: .make(state: .starting, autoStop: nil), history: LiveBarsHistory(),
          elapsedText: nil, open: {}, keepRecording: {}, stop: {})
        BubbleBody(
          presentation: .make(state: .recording(since: since), autoStop: nil),
          history: sampleHistory, elapsedText: "12:34", open: {}, keepRecording: {}, stop: {})
        BubbleBody(
          presentation: .make(
            state: .recording(since: since),
            autoStop: AutoStopPresentation(
              appName: "Zoom", remainingText: "1:29", fractionRemaining: 0.98)),
          history: sampleHistory, elapsedText: "12:34", open: {}, keepRecording: {}, stop: {})
        BubbleBody(
          presentation: .make(state: .stopping, autoStop: nil), history: LiveBarsHistory(),
          elapsedText: nil, open: {}, keepRecording: {}, stop: {})
      }
    }
  }

  #Preview("Recording bubble") {
    PreviewPair { BubblePreviews() }
      .frame(width: 1040, height: 360)
  }
#endif
