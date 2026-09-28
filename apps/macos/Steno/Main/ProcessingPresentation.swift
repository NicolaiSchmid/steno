import StenoCore
import SwiftUI

/// What the processing card shows between two `progress` events: a pure
/// function of the last event and the time since it, sampled once a second
/// by `ProcessingCard`. Core posts the fraction, where the next event is
/// expected and how long the run still needs; this moves the bar towards
/// the next event, counts the estimate down and words it. Nothing here
/// divides an estimate: `expectedTimeToNextEvent` is core's.
enum ProgressPresentation {
  struct State: Equatable {
    /// The bar's value: the event's `fraction` at zero elapsed, then
    /// linearly to `nextFraction - boundaryGap` at `expectedTimeToNextEvent`,
    /// resting there. Never below the event's fraction, so the bar reaches a
    /// boundary only when its event lands and never moves backwards.
    var fraction: Double
    /// The trailing text of the card's title row, from the table in the
    /// plan's D3.
    var remainingText: String
    /// True once the elapsed time passed `slowFactor` times the expected
    /// time to the next event; the text then says so instead of a number.
    var isSlow: Bool
  }

  /// How far short of the next event's fraction the bar stops until the
  /// event lands.
  static let boundaryGap = 0.01
  /// Elapsed over expected time to the next event beyond which a stage
  /// counts as slow.
  static let slowFactor = 1.5

  /// `reduceMotion` is the view's setting passed through so a test pins
  /// that it changes how the bar moves between samples, not what a sample
  /// says: the 1 Hz steps stay, the tween goes (`tween(reduceMotion:)`).
  static func state(progress: ProcessingProgress, elapsed: Duration, reduceMotion: Bool) -> State {
    let expected = progress.expectedTimeToNextEvent
    let isSlow = elapsed > expected * slowFactor
    let remaining = max(.zero, progress.estimatedRemaining - elapsed)
    return State(
      fraction: fraction(progress, elapsed: elapsed),
      remainingText: remainingText(
        remaining, isSlow: isSlow, isEstimateSeeded: progress.isEstimateSeeded),
      isSlow: isSlow)
  }

  /// The animation between two samples: `Motion.countdown`, which lasts
  /// exactly one sampling period, or none under Reduce Motion.
  static func tween(reduceMotion: Bool) -> Animation? {
    reduceMotion ? nil : Motion.countdown
  }

  /// The card's title row as text: the entry's title, then the remaining
  /// text once the run has posted an event. `TabText` renders these as the
  /// pending lines of a tab without content.
  static func lines(entry: ProcessingProgressModel.Entry, elapsed: Duration) -> [String] {
    guard let progress = entry.progress else { return [entry.title] }
    return [
      entry.title,
      state(progress: progress, elapsed: elapsed, reduceMotion: false).remainingText,
    ]
  }

  private static func fraction(_ progress: ProcessingProgress, elapsed: Duration) -> Double {
    let start = progress.fraction
    let target = max(start, progress.nextFraction - boundaryGap)
    let expected = progress.expectedTimeToNextEvent
    guard expected > .zero else { return target }
    let share = min(1, max(0, elapsed / expected))
    return start + (target - start) * share
  }

  /// The plan's D3 table, conditions checked in its order. `remaining` is
  /// the estimate less the elapsed time, already floored at zero.
  private static func remainingText(_ remaining: Duration, isSlow: Bool, isEstimateSeeded: Bool)
    -> String
  {
    if isSlow { return "a bit longer than usual" }
    let minutes = Int((remaining / .seconds(60)).rounded(.up))
    if isEstimateSeeded {
      return remaining < .seconds(90) ? "about a minute" : "about \(minutes) min"
    }
    if remaining < .seconds(10) { return "a few seconds" }
    if remaining < .seconds(60) { return "less than a minute" }
    return "~\(minutes) min remaining"
  }
}

/// The card every content tab shows at the top of its reading column while
/// the meeting is queued or processing, and the Scratchpad above its
/// editor: the stage, the time left, a bar that moves between events, and
/// the privacy line. A `TimelineView` anchored on the entry's `since`
/// samples `ProgressPresentation` on every whole second after the last
/// event, as the menu bar samples its elapsed time; the fill tweens to each
/// sample with `Motion.countdown` unless Reduce Motion is on. Before the
/// run's first event the title is "Waiting to process" and the bar pulses,
/// still under Reduce Motion. Failure is not this card: the meeting leaves
/// the model and the header shows the failed state.
struct ProcessingCard: View {
  let entry: ProcessingProgressModel.Entry
  let meeting: Meeting
  @Environment(\.accessibilityReduceMotion) private var reduceMotion
  @State private var pulsing = false

  /// The bar's geometry from the plan's D4 table.
  static let barHeight: CGFloat = 4
  static let barRadius: CGFloat = 2

  var body: some View {
    Card {
      VStack(alignment: .leading, spacing: Theme.Space.sm) {
        TimelineView(.periodic(from: entry.since, by: Motion.durationCountdown)) { context in
          let state = entry.progress.map {
            ProgressPresentation.state(
              progress: $0, elapsed: elapsed(at: context.date), reduceMotion: reduceMotion)
          }
          VStack(alignment: .leading, spacing: Theme.Space.sm) {
            titleRow(state)
            bar(state)
          }
        }
        HStack(spacing: Theme.Space.sm) {
          Text("Audio stays on this Mac.")
          Text(meeting.source.label)
          if meeting.duration > 0 { Text(meeting.duration.clockText) }
        }
        .font(.steno(Theme.TextSize.xxs))
        .foregroundStyle(Color.stenoFaint)
      }
    }
    .accessibilityElement(children: .contain)
    .accessibilityIdentifier("processing-card")
  }

  /// Time since the last event, never negative when the sample lands on the
  /// anchor itself.
  private func elapsed(at date: Date) -> Duration {
    .seconds(max(0, date.timeIntervalSince(entry.since)))
  }

  private func titleRow(_ state: ProgressPresentation.State?) -> some View {
    HStack(alignment: .firstTextBaseline, spacing: Theme.Space.sm) {
      Text(entry.title)
        .font(.steno(Theme.TextSize.sm, weight: .medium))
        .foregroundStyle(Color.stenoStrong)
        .accessibilityIdentifier("processing-stage")
      Spacer()
      if let state {
        Text(state.remainingText)
          .font(.steno(Theme.TextSize.xs).monospacedDigit())
          .foregroundStyle(Color.stenoMutedForeground)
          .accessibilityIdentifier("processing-remaining")
      }
    }
  }

  /// Track and fill; the fill's width is the sampled fraction, its
  /// accessibility value the same fraction in percent. Without a state the
  /// fill spans the track and pulses between full and `Motion.pulseOpacity`,
  /// or sits dimmed under Reduce Motion.
  private func bar(_ state: ProgressPresentation.State?) -> some View {
    GeometryReader { proxy in
      ZStack(alignment: .leading) {
        RoundedRectangle(cornerRadius: Self.barRadius, style: .continuous)
          .fill(Color.stenoSecondary)
        if let state {
          RoundedRectangle(cornerRadius: Self.barRadius, style: .continuous)
            .fill(Color.stenoStrong)
            .frame(width: proxy.size.width * state.fraction)
            .animation(
              ProgressPresentation.tween(reduceMotion: reduceMotion), value: state.fraction)
        } else {
          RoundedRectangle(cornerRadius: Self.barRadius, style: .continuous)
            .fill(Color.stenoStrong)
            .opacity(pulsing || reduceMotion ? Motion.pulseOpacity : 1)
            .onAppear {
              guard !reduceMotion else { return }
              withAnimation(Motion.pulse.repeatForever(autoreverses: true)) { pulsing = true }
            }
        }
      }
    }
    .frame(height: Self.barHeight)
    .accessibilityElement(children: .ignore)
    .accessibilityLabel("Processing progress")
    .accessibilityValue("\(Int(((state?.fraction ?? 0) * 100).rounded())) percent")
    .accessibilityIdentifier("processing-bar")
  }
}
