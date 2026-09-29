import StenoCore
import SwiftUI

/// Segments grouped into turns: consecutive segments by the same speaker
/// under one `Name — HH:MM:SS` header. The text is display only; the name
/// is a `SpeakerPicker` that opens its own popover, one at a time
/// (`presentedSpeakerID`), so a speaker can be renamed where they are read.
/// Closing a picker re-exports when something changed.
struct TranscriptTab: View {
  let model: MeetingDetailViewModel
  /// Where the pipeline is with this meeting while it is queued or
  /// processing, from `controller.progress.entry(for:)`; nil otherwise. The
  /// card it drives replaces the pending copy and the spinner.
  let progress: ProcessingProgressModel.Entry?
  @State private var presentedSpeakerID: UUID?

  /// The gap between turns; the plan's 20 pt.
  static let turnGap: CGFloat = 20

  var body: some View {
    let turns = TranscriptTurns.group(model.export?.segments ?? [])
    if turns.isEmpty, progress == nil {
      PendingText(tab: .transcript, model: model)
    } else {
      ScrollView {
        LazyVStack(alignment: .leading, spacing: Self.turnGap) {
          ProcessingCardSlot(progress: progress, meeting: model.meeting)
          ForEach(turns) { turn in
            turnView(turn)
          }
        }
        .readingColumn()
      }
    }
  }

  /// One turn: the speaker (a picker), the timestamp 12 mono `muted`, the
  /// lane as a neutral chip only for a call (in person has one lane), then
  /// the text at 14/19.
  private func turnView(_ turn: TranscriptTurns.Turn) -> some View {
    VStack(alignment: .leading, spacing: Theme.Space.xs) {
      HStack(spacing: Theme.Space.sm) {
        if let speakerID = turn.speakerID {
          SpeakerPicker(
            model: model.speakers, speakerID: speakerID,
            isExpanded: Binding(
              get: { presentedSpeakerID == speakerID },
              set: { open in
                if open {
                  presentedSpeakerID = speakerID
                } else if presentedSpeakerID == speakerID {
                  presentedSpeakerID = nil
                  Task { await model.pickerClosed() }
                }
              }),
            presentation: .popover)
        } else {
          Text(model.displayName(forSpeaker: nil))
            .font(.steno(Theme.TextSize.xs, weight: .semibold))
            .foregroundStyle(Color.stenoStrong)
        }
        Text(turn.start.timestampText)
          .font(.steno(Theme.TextSize.xxs).monospacedDigit())
          .foregroundStyle(Color.stenoMutedForeground)
        if model.meeting?.source == .macCall {
          StatusChip(text: turn.lane.label, style: .neutral)
        }
      }
      Text(turn.text)
        .font(.steno(Theme.TextSize.sm))
        .proseLeading()
        .foregroundStyle(Color.stenoForeground)
        .fixedSize(horizontal: false, vertical: true)
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
