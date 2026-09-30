import StenoAudio
import StenoCore
import SwiftUI

// The recorder's views shared by the sidebar control and the menu bar item:
// the level bars, the ticking elapsed time, the Stop label and the message
// rows. Recording is one hue (the red dot); the meters are achromatic.

/// Thin level bars, one per lane, from the 10 Hz `LaneLevels`. `strong`
/// over a `border` track on every surface. In person the one lane is the
/// room, so `mic` carries `AudioLane.mixed` and is labelled as such.
struct LevelBars: View {
  let levels: LaneLevels

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.xs) {
      bar(levels.system == nil ? AudioLane.mixed.label : AudioLane.mic.label, levels.mic)
      if let system = levels.system {
        bar(AudioLane.system.label, system)
      }
    }
  }

  private func bar(_ label: String, _ level: LaneLevel) -> some View {
    HStack(spacing: Theme.Space.sm) {
      Text(label)
        .font(.steno(Theme.TextSize.xxxs))
        .foregroundStyle(Color.stenoFaint)
        .frame(width: 44, alignment: .leading)
      GeometryReader { proxy in
        ZStack(alignment: .leading) {
          Capsule().fill(Color.stenoBorder)
          Capsule()
            .fill(Color.stenoStrong)
            .frame(width: proxy.size.width * Self.fraction(level.rms))
            .animation(Motion.functional, value: level.rms)
        }
      }
      .frame(height: 4)
    }
    .accessibilityElement(children: .ignore)
    .accessibilityLabel("\(label) level")
    .accessibilityValue("\(Int(Self.fraction(level.rms) * 100)) percent")
  }

  /// dBFS from -60 to 0 mapped onto 0...1. `nonisolated`: `View` conformance
  /// makes the struct main-actor isolated, and `LiveBarsHistory` calls this
  /// from plain code.
  nonisolated static func fraction(_ dbfs: Float) -> CGFloat {
    CGFloat(min(1, max(0, (dbfs + 60) / 60)))
  }
}

/// The elapsed recording time, ticking once a second from `since` in
/// `mm:ss` (`h:mm:ss` past an hour). No timer state on any model.
struct ElapsedText: View {
  let since: Date

  var body: some View {
    TimelineView(.periodic(from: since, by: 1)) { context in
      Text(context.date.timeIntervalSince(since).clockText)
        .monospacedDigit()
    }
  }
}

/// The Stop control's label: the destructive dot, "Stop" and the elapsed
/// time. Sits in a `StenoSecondaryButtonStyle` button on both surfaces.
struct StopLabel: View {
  let since: Date

  var body: some View {
    HStack(spacing: Theme.Space.sm) {
      StatusDot(color: .stenoDestructive)
      Text("Stop")
        .foregroundStyle(Color.stenoDestructive)
      ElapsedText(since: since)
        .foregroundStyle(Color.stenoMutedForeground)
    }
  }
}

/// What a Stop control renders while the recorder holds a meeting: enabled
/// while recording and disabled (with a spinner) while the stop is
/// finishing. Nil for every meeting the recorder is not on, so a
/// `.recording` row the recorder does not yet hold (while `.starting`;
/// never after `reconcileInterruptedRecordings`) shows nothing in its
/// place. The sidebar builds the same value from `RecordingState` alone.
enum HeaderStop: Equatable, Sendable {
  case stop(since: Date)
  case stopping

  static func make(meetingID: UUID, recording: RecordingState, activeMeetingID: UUID?)
    -> HeaderStop?
  {
    guard activeMeetingID == meetingID else { return nil }
    switch recording {
    case .recording(let since): return .stop(since: since)
    case .stopping: return .stopping
    case .idle, .starting: return nil
    }
  }
}

/// The Stop control both surfaces share: a button carrying `StopLabel` while
/// recording, with `id` naming the surface (`sidebar-stop`, `header-stop`),
/// and a disabled button holding a spinner in its place while the stop is
/// finishing, so the control keeps its box. The header's is a secondary
/// button hugging its label; the sidebar's is the 40 pt `StopControlStyle`
/// box across the nav column with the compact level meter trailing.
struct StopButton: View {
  enum Surface {
    case header
    case sidebar(levels: LaneLevels?)
  }

  let state: HeaderStop
  let surface: Surface
  let id: String
  let stop: () -> Void

  var body: some View {
    switch state {
    case .stop(let since):
      styled(Button(action: stop) { label(since: since) })
        .help("Stop recording (⌘⇧R)")
        .accessibilityIdentifier(id)
    case .stopping:
      styled(
        Button(action: {}) {
          ProgressView()
            .controlSize(.small)
            .frame(maxWidth: fillsWidth ? .infinity : nil)
        }
      )
      .disabled(true)
      .accessibilityLabel(RecordingState.stopping.label)
    }
  }

  private var fillsWidth: Bool {
    if case .sidebar = surface { return true }
    return false
  }

  @ViewBuilder
  private func label(since: Date) -> some View {
    switch surface {
    case .header:
      StopLabel(since: since)
    case .sidebar(let levels):
      // The label never wraps (a clock split over two lines read as
      // "00:0 / 2" in the 200 pt column); the meter is what gives way when
      // the box cannot hold both.
      ViewThatFits(in: .horizontal) {
        HStack(spacing: Theme.Space.sm) {
          StopLabel(since: since).fixedSize()
          Spacer(minLength: Theme.Space.sm)
          if let levels {
            CompactLevelBars(levels: levels)
          }
        }
        StopLabel(since: since).fixedSize()
      }
      .padding(.horizontal, Theme.Control.buttonInset)
    }
  }

  @ViewBuilder
  private func styled(_ button: Button<some View>) -> some View {
    switch surface {
    case .header: button.buttonStyle(StenoSecondaryButtonStyle())
    case .sidebar: button.buttonStyle(StopControlStyle())
    }
  }
}

/// The recording box: the CTA's 40 pt and radius 12 on a `raised` surface
/// with a hairline, the `card` veil on hover, 14 pt medium; destructive is
/// the dot and the word inside `StopLabel`, never a fill.
private struct StopControlStyle: ButtonStyle {
  func makeBody(configuration: Configuration) -> some View {
    Box(configuration: configuration)
  }

  private struct Box: View {
    let configuration: Configuration
    @State private var hovering = false
    @Environment(\.isEnabled) private var isEnabled
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
      configuration.label
        .font(.steno(Theme.TextSize.sm, weight: .medium))
        .foregroundStyle(Color.stenoStrong)
        .frame(height: Theme.Control.ctaHeight)
        .frame(maxWidth: .infinity)
        .background(
          ZStack {
            Theme.Radius.lg.shape.fill(Color.stenoRaised)
            Theme.Radius.lg.shape.fill(hovering && isEnabled ? Color.stenoCard : Color.clear)
          }
        )
        .overlay(Theme.Radius.lg.shape.hairline())
        .contentShape(Theme.Radius.lg.shape)
        .onHover { hovering = $0 }
        .animation(Motion.swap(reduceMotion: reduceMotion), value: hovering)
        .opacity(configuration.isPressed ? Motion.controlPressOpacity : 1)
        .opacity(isEnabled ? 1 : Motion.disabledOpacity)
        .animation(Motion.swap(reduceMotion: reduceMotion), value: configuration.isPressed)
    }
  }
}

/// The level meter inside the Stop control: one 4 pt bar per lane, 40 pt
/// wide, 4 pt apart (12 pt tall for a call), `strong` over a `border`
/// track, no labels. The labelled `LevelBars` stay on the menu bar item.
private struct CompactLevelBars: View {
  let levels: LaneLevels

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.xs) {
      bar(levels.mic)
      if let system = levels.system {
        bar(system)
      }
    }
    .frame(width: Theme.Control.meterWidth)
    .accessibilityElement(children: .ignore)
    .accessibilityLabel("Input level")
    .accessibilityValue("\(Int(LevelBars.fraction(levels.mic.rms) * 100)) percent")
  }

  private func bar(_ level: LaneLevel) -> some View {
    ZStack(alignment: .leading) {
      Capsule().fill(Color.stenoBorder)
      Capsule()
        .fill(Color.stenoStrong)
        .frame(width: Theme.Control.meterWidth * LevelBars.fraction(level.rms))
        .animation(Motion.functional, value: level.rms)
    }
    .frame(height: Theme.Control.meterHeight)
  }
}

/// The armed auto-stop under the Stop control on both surfaces: the one
/// countdown line the recorder exposes as a warning row, and "Keep
/// recording" next to it. The Stop control right above is the other choice,
/// so the two carry equal weight without a second stop button here.
struct AutoStopRow: View {
  let presentation: AutoStopPresentation
  /// The button's accessibility identifier, one per surface.
  let identifier: String
  let keepRecording: () -> Void

  var body: some View {
    HStack(alignment: .firstTextBaseline, spacing: Theme.Space.sm) {
      MessageRow(kind: .warning, text: presentation.line)
      Spacer(minLength: 0)
      Button(AutoStopPresentation.keepRecordingLabel, action: keepRecording)
        .buttonStyle(StenoSecondaryButtonStyle())
        .fixedSize()
        .accessibilityIdentifier(identifier)
    }
  }
}

/// The recorder's warning and error, as rows; both surfaces show the same
/// text and the recorder clears them on the next start. Takes the strings,
/// so the menu bar can fall back to its own model's error.
struct RecordingMessages: View {
  let warning: String?
  let error: String?

  var body: some View {
    if let warning {
      MessageRow(kind: .warning, text: warning)
    }
    if let error {
      MessageRow(kind: .error, text: error)
    }
  }
}
