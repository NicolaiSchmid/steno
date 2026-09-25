import StenoCore
import SwiftUI

/// Segments grouped into turns: consecutive segments by the same speaker
/// under one `Name — HH:MM:SS` header. Display only.
struct TranscriptTab: View {
  let model: MeetingDetailViewModel

  var body: some View {
    ScrollView {
      LazyVStack(alignment: .leading, spacing: Theme.Space.lg) {
        let turns = TranscriptTurns.group(model.export?.segments ?? [])
        if turns.isEmpty {
          Text(
            model.meeting?.state == .ready
              ? "No transcript" : "Transcript appears after processing"
          )
          .font(.steno(Theme.TextSize.sm, weight: .medium))
          .foregroundStyle(Color.stenoMutedForeground)
        }
        ForEach(turns) { turn in
          VStack(alignment: .leading, spacing: Theme.Space.xs) {
            HStack(spacing: Theme.Space.sm) {
              Text(model.displayName(forSpeaker: turn.speakerID))
                .font(.steno(Theme.TextSize.xs, weight: .semibold))
                .foregroundStyle(Color.stenoStrong)
              Text(turn.start.timestampText)
                .font(.steno(Theme.TextSize.xxs).monospacedDigit())
                .foregroundStyle(Color.stenoFaint)
              Text(turn.lane.rawValue)
                .font(.steno(Theme.TextSize.xxxs))
                .foregroundStyle(Color.stenoGhost)
            }
            Text(turn.text)
              .font(.steno(Theme.TextSize.sm))
              .foregroundStyle(Color.stenoForeground)
              .fixedSize(horizontal: false, vertical: true)
          }
        }
      }
      .frame(maxWidth: 720, alignment: .leading)
      .padding(Theme.Space.lg)
      .textSelection(.enabled)
    }
  }
}

/// Pure grouping of segments into speaker turns.
enum TranscriptTurns {
  struct Turn: Identifiable, Equatable {
    var id: UUID
    var speakerID: UUID?
    var lane: AudioLane
    var start: TimeInterval
    var text: String
  }

  static func group(_ segments: [TranscriptSegment]) -> [Turn] {
    var turns: [Turn] = []
    for segment in segments.sorted(by: { $0.start < $1.start }) {
      if let last = turns.last, last.speakerID == segment.speakerID, last.lane == segment.lane,
        segment.speakerID != nil
      {
        turns[turns.count - 1].text += " " + segment.text
      } else {
        turns.append(
          Turn(
            id: segment.id, speakerID: segment.speakerID, lane: segment.lane,
            start: segment.start, text: segment.text))
      }
    }
    return turns
  }
}
