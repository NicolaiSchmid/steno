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

  /// dBFS from -60 to 0 mapped onto 0...1.
  static func fraction(_ dbfs: Float) -> CGFloat {
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
