import StenoCore
import SwiftUI

/// The sidebar list with search, a state filter, a tag filter, and delete
/// (context menu and toolbar, behind a confirmation).
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
            .contextMenu {
              Button("Delete Meeting…", role: .destructive) { model.pendingDeletion = meeting }
                .disabled(!Self.canDelete(meeting))
            }
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
    .toolbar {
      ToolbarItem {
        Button {
          model.pendingDeletion = selectedMeeting
        } label: {
          Label("Delete Meeting", systemImage: "trash")
        }
        .disabled(selectedMeeting.map { !Self.canDelete($0) } ?? true)
        .help("Delete the selected meeting and its recording")
        .accessibilityIdentifier("delete-meeting")
      }
    }
    .confirmationDialog(
      "Delete “\(model.pendingDeletion?.title ?? "")”?",
      isPresented: Binding(
        get: { model.pendingDeletion != nil },
        set: { if !$0 { model.pendingDeletion = nil } }),
      titleVisibility: .visible
    ) {
      Button("Delete", role: .destructive) {
        guard let meeting = model.pendingDeletion else { return }
        Task { await model.delete(meeting.id) }
      }
      Button("Cancel", role: .cancel) { model.pendingDeletion = nil }
    } message: {
      Text(
        "The transcript, summary, tasks and the recording on this Mac are removed. Files already exported to Obsidian stay. People stay."
      )
    }
  }

  private var selectedMeeting: Meeting? {
    guard let selection = model.selection else { return nil }
    return model.all.first { $0.id == selection }
  }

  /// The store refuses while the capture writer or the pipeline holds the
  /// meeting's files; the controls say so before the attempt.
  static func canDelete(_ meeting: Meeting) -> Bool {
    switch meeting.state {
    case .recording, .processing: false
    case .queued, .ready, .failed: true
    }
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
