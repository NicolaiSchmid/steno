import StenoCore
import SwiftUI

/// One card per calendar day: the date and weekday as its header, then one
/// `MeetingEntry` per meeting. The entries reach 8 pt from the card's edge
/// so their rails line up with the header text. VoiceOver reads the card as
/// a group named by its date, then the entries.
struct MeetingCard: View {
  let group: MeetingListViewModel.DayGroup
  let selection: UUID?
  /// Whether the list column has keyboard focus; the selected entry shows
  /// it.
  let listFocused: Bool
  let calendar: Calendar
  /// The progress model's stage title for a queued or processing meeting;
  /// nil otherwise.
  let statusLine: (Meeting) -> String?
  let select: (UUID) -> Void
  let delete: (Meeting) -> Void

  private var dateText: String { DisplayFormat.monthDay(group.day, calendar: calendar) }
  private var weekdayText: String { DisplayFormat.weekday(group.day, calendar: calendar) }

  var body: some View {
    Card {
      VStack(alignment: .leading, spacing: Theme.Space.md) {
        header
        VStack(spacing: Theme.Space.md) {
          ForEach(group.meetings) { meeting in
            MeetingEntry(
              meeting: meeting, isSelected: meeting.id == selection,
              isFocused: listFocused && meeting.id == selection, calendar: calendar,
              statusLine: statusLine(meeting), select: { select(meeting.id) },
              delete: { delete(meeting) }
            )
            .id(meeting.id)
          }
        }
        .padding(.horizontal, -Theme.Space.sm)
      }
    }
    .accessibilityElement(children: .contain)
    .accessibilityLabel("\(dateText), \(weekdayText)")
  }

  /// "Sep 24 / Thursday": the date `strong` and semibold, the weekday `muted`.
  private var header: some View {
    (Text(dateText)
      .font(.steno(Theme.TextSize.xxs, weight: .semibold))
      .foregroundStyle(Color.stenoStrong)
      + Text(" / \(weekdayText)")
      .font(.steno(Theme.TextSize.xxs))
      .foregroundStyle(Color.stenoMutedForeground))
      .lineLimit(1)
  }
}

/// One meeting in a card: a 2 pt rail, the display title, the start time
/// with a chip only for queued, processing and failed, and a one-line
/// preview (the live entry's elapsed time comes with step 7a). The whole
/// 8 pt padded frame is the click target; selection is the `strong` rail
/// and a `secondary` veil, hover a `card` veil, both over
/// `Motion.functional`; while the list has keyboard focus the selected
/// entry wears the `ring` hairline the search field uses, so focus reads as
/// one rule. No system selection colour anywhere. Id `meeting-<uuid>`, the
/// state word as the accessibility value, the `isSelected` trait while
/// selected.
struct MeetingEntry: View {
  let meeting: Meeting
  let isSelected: Bool
  /// The selected entry while the list column has keyboard focus.
  let isFocused: Bool
  let calendar: Calendar
  let statusLine: String?
  let select: () -> Void
  let delete: () -> Void
  @State private var hovering = false
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  private var isRecording: Bool { meeting.state == .recording }

  private var showsChip: Bool {
    switch meeting.state {
    case .queued, .processing, .failed: true
    case .recording, .ready: false
    }
  }

  private var veil: Color { Color.stenoRowVeil(isSelected: isSelected, hovering: hovering) }

  var body: some View {
    Button(action: select) {
      HStack(alignment: .top, spacing: Theme.Space.md) {
        RoundedRectangle(cornerRadius: Theme.Space.hairline)
          .fill(isSelected ? Color.stenoStrong : Color.stenoBorder)
          .frame(width: Theme.Control.railWidth)
        VStack(alignment: .leading, spacing: Theme.Space.xxs) {
          HStack(alignment: .firstTextBaseline, spacing: Theme.Space.sm) {
            Text(meeting.displayTitle(calendar: calendar))
              .font(.steno(Theme.TextSize.sm, weight: .medium))
              .foregroundStyle(Color.stenoStrong)
              .lineLimit(1)
              .truncationMode(.tail)
            Spacer(minLength: 0)
            if showsChip {
              StatusChip(meeting.state)
            }
          }
          HStack(spacing: Theme.Space.sm) {
            Text(DisplayFormat.time(meeting.startedAt, calendar: calendar))
              .monospacedDigit()
            if isRecording {
              PulsingDot()
              Text(MeetingState.recording.label)
            }
          }
          .font(.steno(Theme.TextSize.xxs))
          .foregroundStyle(Color.stenoMutedForeground)
          if !isRecording {
            Text(statusLine ?? meeting.previewLine)
              .font(.steno(Theme.TextSize.xs))
              .foregroundStyle(Color.stenoMutedForeground)
              .lineLimit(1)
              .truncationMode(.tail)
          }
        }
      }
      .padding(Theme.Space.sm)
      .frame(maxWidth: .infinity, alignment: .leading)
      .background(Theme.Radius.md.shape.fill(veil))
      .overlay(Theme.Radius.md.shape.hairline(isFocused ? Color.stenoRing : Color.clear))
      .contentShape(Theme.Radius.md.shape)
    }
    .buttonStyle(.plain)
    .onHover { hovering = $0 }
    .animation(Motion.swap(reduceMotion: reduceMotion), value: hovering)
    .animation(Motion.swap(reduceMotion: reduceMotion), value: isSelected)
    .animation(Motion.swap(reduceMotion: reduceMotion), value: isFocused)
    .contextMenu {
      Button("Delete Meeting…", role: .destructive, action: delete)
        .disabled(!MeetingListViewModel.canDelete(meeting))
    }
    .accessibilityIdentifier("meeting-\(meeting.id.uuidString)")
    .accessibilityValue(meeting.state.label)
    .accessibilityAddTraits(isSelected ? [.isSelected] : [])
  }
}

/// The one ambient animation in the window: the live entry's `destructive`
/// dot breathing over `Motion.pulse`. Holds at full opacity under Reduce
/// Motion.
private struct PulsingDot: View {
  @State private var dimmed = false
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  var body: some View {
    StatusDot(color: Color.stenoDestructive)
      .opacity(dimmed ? Motion.pulseOpacity : 1)
      .onAppear {
        guard !reduceMotion else { return }
        withAnimation(Motion.pulse.repeatForever(autoreverses: true)) { dimmed = true }
      }
      .accessibilityHidden(true)
  }
}

#if DEBUG
  /// The rich seed as cards, the fixture selected, in UTC so the preview
  /// does not move with the machine.
  private struct MeetingCardsPreview: View {
    private static let utc: Calendar = {
      var calendar = Calendar(identifier: .gregorian)
      calendar.timeZone = TimeZone(identifier: "UTC")!
      return calendar
    }()

    private var groups: [MeetingListViewModel.DayGroup] {
      let meetings = [SampleData.meeting()] + PreviewSeed.richMeetings()
      return MeetingListViewModel.DayGroup.group(meetings, calendar: Self.utc)
    }

    var body: some View {
      ScrollView {
        LazyVStack(spacing: Theme.Space.md) {
          ForEach(groups) { group in
            MeetingCard(
              group: group, selection: SampleData.meetingID, listFocused: true,
              calendar: Self.utc,
              statusLine: { $0.state == .processing ? "Transcribing…" : nil },
              select: { _ in }, delete: { _ in })
          }
        }
      }
      .frame(width: 348)
    }
  }

  #Preview("Meeting cards") {
    PreviewPair { MeetingCardsPreview() }
      .frame(width: 800, height: 640)
  }
#endif
