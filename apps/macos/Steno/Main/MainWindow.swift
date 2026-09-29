import StenoCore
import SwiftUI

/// The main window: nav column, list column, detail. Three columns in the
/// balanced style with the visibility held at `.all`, because the nav
/// column holds the window's only Record control and must never collapse:
/// a drag past the first divider's minimum still collapses the AppKit
/// column, so the state is reverted to `.all` as soon as it changes, and
/// the column minimums (200 + 320 + 440) add up to the window's 960 pt
/// minimum so the layout is never over-constrained. The ideals add up to
/// it too: the split view lays the content and detail columns out at their
/// ideals before it honours the sidebar's minimum, and 220 + 380 + 440
/// squeezed the nav column to 140 pt at 960, truncating the Record control
/// and every nav row. No title, no toolbar items: the window style hides
/// the title bar and each column paints its own opaque background. One
/// detail view model per selected meeting, replaced when the selection
/// changes.
struct MainWindow: View {
  let controller: AppController
  @State private var list: MeetingListViewModel
  @State private var detail: MeetingDetailViewModel?
  @State private var columns: NavigationSplitViewVisibility = .all

  init(controller: AppController) {
    self.controller = controller
    _list = State(
      initialValue: MeetingListViewModel(
        store: controller.environment.store, clock: controller.environment.clock))
  }

  var body: some View {
    NavigationSplitView(columnVisibility: $columns) {
      NavigationColumn(controller: controller, list: list)
        .navigationSplitViewColumnWidth(min: 200, ideal: 200, max: 260)
        .toolbar(removing: .sidebarToggle)
    } content: {
      MeetingListView(model: list, progress: controller.progress)
        .navigationSplitViewColumnWidth(min: 320, ideal: 320, max: 480)
    } detail: {
      // Row 1 of the detail header stack: the setup banner, over the
      // selected meeting or the empty state alike.
      VStack(spacing: 0) {
        SetupBanner(controller: controller, hasMeetings: !list.all.isEmpty)
        if let detail {
          MeetingDetailView(model: detail, controller: controller)
            .id(detail.id)
        } else {
          EmptyState(
            symbol: "text.alignleft", title: "Select a meeting",
            body: "Pick a meeting on the left to read its summary, transcript and tasks.",
            id: "empty-detail")
        }
      }
      .frame(minWidth: 440, maxWidth: .infinity, maxHeight: .infinity)
      .background(Color.stenoBackground)
    }
    .navigationSplitViewStyle(.balanced)
    .toolbar(removing: .title)
    .toolbarBackground(.hidden, for: .windowToolbar)
    .frame(minWidth: 960, minHeight: 600)
    .onChange(of: columns) { _, visibility in
      // A collapsed nav column would hide the Record control; bring it back.
      if visibility != .all { columns = .all }
    }
    .task { await list.observe() }
    .onChange(of: list.selection, initial: true) { _, selection in
      guard selection != detail?.id else { return }
      detail = selection.map {
        MeetingDetailViewModel(
          meetingID: $0, environment: controller.environment,
          initialSettings: controller.storedSettings)
      }
    }
    .onChange(of: controller.requestedMeetingID, initial: true) { _, requested in
      guard let requested else { return }
      list.selection = requested
      controller.requestedMeetingID = nil
    }
    .onChange(of: list.all.isEmpty, initial: true) { _, empty in
      // First launch of the window: show the newest meeting.
      if !empty, list.selection == nil, let first = list.meetings.first {
        list.selection = first.id
      }
    }
  }
}
