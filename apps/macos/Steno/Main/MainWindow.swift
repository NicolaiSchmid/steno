import StenoCore
import SwiftUI

/// The main window: sidebar list plus detail. One detail view model per
/// selected meeting, replaced when the selection changes.
struct MainWindow: View {
  let controller: AppController
  @State private var list: MeetingListViewModel
  @State private var detail: MeetingDetailViewModel?

  init(controller: AppController) {
    self.controller = controller
    _list = State(
      initialValue: MeetingListViewModel(
        store: controller.environment.store, clock: controller.environment.clock))
  }

  var body: some View {
    NavigationSplitView {
      MeetingListView(model: list)
        .navigationSplitViewColumnWidth(min: 240, ideal: 300, max: 420)
    } detail: {
      if let detail {
        MeetingDetailView(model: detail, controller: controller)
          .id(detail.id)
      } else {
        VStack(spacing: Theme.Space.sm) {
          Image(systemName: "text.alignleft")
            .font(.system(size: 28))
            .foregroundStyle(Color.stenoGhost)
          Text("Select a meeting")
            .font(.steno(Theme.TextSize.sm, weight: .medium))
            .foregroundStyle(Color.stenoMutedForeground)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.stenoBackground)
      }
    }
    .navigationTitle("Steno")
    .frame(minWidth: 820, minHeight: 520)
    .onChange(of: list.selection, initial: true) { _, selection in
      guard selection != detail?.id else { return }
      detail = selection.map {
        MeetingDetailViewModel(meetingID: $0, environment: controller.environment)
      }
    }
    .onChange(of: controller.requestedMeetingID, initial: true) { _, requested in
      guard let requested else { return }
      list.select(requested)
      controller.requestedMeetingID = nil
    }
    .onChange(of: list.all.isEmpty, initial: true) { _, empty in
      // First launch of the window: show the newest meeting.
      if !empty, list.selection == nil, let first = list.meetings.first {
        list.select(first.id)
      }
    }
  }
}
