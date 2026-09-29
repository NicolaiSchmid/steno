import StenoCore
import SwiftUI

/// The list column: the heading (the tag, the state filter or "Meetings"),
/// the search field, then the day cards in a `ScrollView`, and delete behind
/// a confirmation (the entry's context menu, the Delete key). Not a `List`:
/// AppKit's row selection follows the system accent and cannot be
/// recoloured, so the entries own their rail-and-veil selection and the
/// column owns the arrow keys. The list takes keyboard focus when the
/// window opens, so the arrows work before a click, and the selected entry
/// wears the `ring` hairline while it has focus. The selection scrolls
/// into view.
struct MeetingListView: View {
  @Bindable var model: MeetingListViewModel
  /// Where the pipeline is with each queued or processing meeting, passed
  /// through from `MainWindow` because the list receives nothing else from
  /// the controller; the entry's preview line reads it.
  let progress: ProcessingProgressModel
  @FocusState private var searchFocused: Bool
  @FocusState private var listFocused: Bool
  @Environment(\.accessibilityReduceMotion) private var reduceMotion

  var body: some View {
    VStack(alignment: .leading, spacing: 0) {
      header
      cards
    }
    .frame(maxWidth: .infinity, maxHeight: .infinity)
    .background(Color.stenoBackground)
    .overlay(alignment: .trailing) {
      Color.stenoBorder.frame(width: Theme.Space.hairline)
    }
    .defaultFocus($listFocused, true)
    .focusedSceneValue(\.searchFocus, SearchFocusAction { searchFocused = true })
    .confirmationDialog(
      "Delete “\(model.pendingDeletion?.displayTitle(calendar: model.calendar) ?? "")”?",
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

  private var header: some View {
    VStack(alignment: .leading, spacing: 0) {
      Text(model.title)
        .font(.steno(Theme.TextSize.lg, weight: .semibold))
        .tracking(-0.2)
        .foregroundStyle(Color.stenoStrong)
        .lineLimit(1)
        .accessibilityAddTraits(.isHeader)
      SearchField(
        text: $model.query, placeholder: "Search meetings", id: "search-meetings",
        focus: $searchFocused
      )
      .padding(.top, Theme.Space.md)
      if let error = model.error {
        MessageRow(kind: .error, text: error)
          .padding(.top, Theme.Space.md)
      }
    }
    .padding(.top, Theme.Space.xl)
    .padding(.horizontal, Theme.Space.lg)
    .padding(.bottom, Theme.Space.lg)
  }

  private var cards: some View {
    listBehaviour(
      ZStack {
        if model.meetings.isEmpty {
          emptyState
            .padding(Theme.Space.lg)
        } else {
          ScrollViewReader { proxy in
            ScrollView {
              LazyVStack(spacing: Theme.Space.md) {
                ForEach(model.dayGroups) { group in
                  MeetingCard(
                    group: group, selection: model.selection, listFocused: listFocused,
                    calendar: model.calendar,
                    statusLine: { progress.entry(for: $0.id)?.title },
                    select: { id in
                      model.selection = id
                      listFocused = true
                    },
                    delete: { model.pendingDeletion = $0 })
                }
              }
              .padding(.horizontal, Theme.Space.lg)
              .padding(.bottom, Theme.Space.xl)
            }
            .onChange(of: model.selection) { _, selection in
              guard let selection else { return }
              withAnimation(reduceMotion ? nil : Motion.spatial) {
                proxy.scrollTo(selection)
              }
            }
          }
        }
      }
      .frame(maxWidth: .infinity, maxHeight: .infinity))
  }

  /// The column's keyboard and accessibility behaviour, kept apart from the
  /// scroll layout: focus without the system ring, the arrow keys, the
  /// Delete key behind `canDelete`, and the labelled container.
  private func listBehaviour(_ content: some View) -> some View {
    content
      .focusable()
      .focusEffectDisabled()
      .focused($listFocused)
      .onMoveCommand { direction in
        switch direction {
        case .down: model.selectNext()
        case .up: model.selectPrevious()
        default: break
        }
      }
      .onDeleteCommand {
        guard let meeting = model.selectedMeeting, MeetingListViewModel.canDelete(meeting)
        else { return }
        model.pendingDeletion = meeting
      }
      .accessibilityElement(children: .contain)
      .accessibilityLabel("Meetings")
      .accessibilityIdentifier("meeting-list")
  }

  @ViewBuilder
  private var emptyState: some View {
    if model.all.isEmpty {
      EmptyState(
        symbol: "waveform", title: "No meetings yet",
        body: "Press Record call above, or ⌘⇧R. The menu bar item works too.",
        id: "empty-meetings")
    } else {
      EmptyState(
        symbol: "magnifyingglass", title: "No meetings match",
        body: "Nothing matches this search or filter.",
        action: .init(title: "Clear filters", id: "clear-filters") { model.clearFilters() },
        id: "empty-meetings")
    }
  }
}

/// The list column's "focus the search field" action, published as a
/// focused scene value while the main window is key, so ⌘F in
/// `AppCommands` reaches the field without the commands knowing the view.
struct SearchFocusAction {
  let run: @MainActor () -> Void
}

private struct SearchFocusKey: FocusedValueKey {
  typealias Value = SearchFocusAction
}

extension FocusedValues {
  var searchFocus: SearchFocusAction? {
    get { self[SearchFocusKey.self] }
    set { self[SearchFocusKey.self] = newValue }
  }
}
