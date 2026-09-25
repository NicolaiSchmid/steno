import StenoCore
import SwiftUI

/// The sidebar list with search, a state filter and a tag filter.
struct MeetingListView: View {
  @Bindable var model: MeetingListViewModel

  var body: some View {
    VStack(spacing: 0) {
      filters
      Divider().overlay(Color.stenoBorder)
      List(selection: $model.selection) {
        ForEach(model.meetings) { meeting in
          MeetingRow(meeting: meeting)
            .tag(meeting.id)
            .listRowSeparator(.hidden)
        }
      }
      .listStyle(.sidebar)
      .scrollContentBackground(.hidden)
      .overlay {
        if model.meetings.isEmpty {
          emptyState
        }
      }
      if let error = model.error {
        MessageRow(kind: .error, text: error).padding(Theme.Space.md)
      }
    }
    .searchable(text: $model.query, placement: .sidebar, prompt: "Search meetings")
    .background(Color.stenoBackground)
  }

  private var filters: some View {
    HStack(spacing: Theme.Space.sm) {
      Picker("State", selection: $model.stateFilter) {
        ForEach(MeetingListViewModel.StateFilter.allCases) { filter in
          Text(filter.title).tag(filter)
        }
      }
      .labelsHidden()
      .pickerStyle(.menu)
      if !model.tags.isEmpty {
        Picker("Tag", selection: $model.tagFilter) {
          Text("All tags").tag(String?.none)
          ForEach(model.tags, id: \.self) { tag in
            Text("#\(tag)").tag(String?.some(tag))
          }
        }
        .labelsHidden()
        .pickerStyle(.menu)
      }
    }
    .padding(.horizontal, Theme.Space.md)
    .padding(.vertical, Theme.Space.sm)
  }

  private var emptyState: some View {
    VStack(spacing: Theme.Space.sm) {
      Image(systemName: "waveform")
        .font(.system(size: 28))
        .foregroundStyle(Color.stenoGhost)
      Text(model.all.isEmpty ? "No meetings yet" : "No meetings match")
        .font(.steno(Theme.TextSize.sm, weight: .medium))
        .foregroundStyle(Color.stenoMutedForeground)
      if model.all.isEmpty {
        Text("Start a recording from the menu bar item.")
          .font(.steno(Theme.TextSize.xs))
          .foregroundStyle(Color.stenoFaint)
      }
    }
    .padding(Theme.Space.xl)
  }
}

struct MeetingRow: View {
  let meeting: Meeting

  var body: some View {
    VStack(alignment: .leading, spacing: Theme.Space.xs) {
      HStack(alignment: .firstTextBaseline) {
        Text(meeting.title)
          .font(.steno(Theme.TextSize.sm, weight: .medium))
          .foregroundStyle(Color.stenoStrong)
          .lineLimit(1)
        Spacer(minLength: Theme.Space.sm)
        StatusChip(meeting.state)
      }
      HStack(spacing: Theme.Space.sm) {
        Text(meeting.startedAt, format: .dateTime.day().month(.abbreviated).hour().minute())
        if meeting.duration > 0 {
          Text(meeting.duration.clockText)
        }
        Text(meeting.source.label)
        if !meeting.tags.isEmpty {
          Text(meeting.tags.map { "#\($0)" }.joined(separator: " "))
            .lineLimit(1)
        }
      }
      .font(.steno(Theme.TextSize.xxs))
      .foregroundStyle(Color.stenoFaint)
    }
    .padding(.vertical, Theme.Space.xs)
    .accessibilityIdentifier("meeting-\(meeting.id.uuidString)")
  }
}

extension MeetingSource {
  var label: String {
    switch self {
    case .macCall: "Call"
    case .macInPerson: "In person"
    case .phone: "Phone"
    }
  }
}
