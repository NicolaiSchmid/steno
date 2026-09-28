import SwiftUI

/// The nav column: the record control pinned at the top, the state filters
/// with counts, the tag filters when tags exist, and Settings pinned at the
/// bottom; the rows in between scroll if the tags overflow. Filters are
/// `NavRow` buttons over the same `stateFilter` and `tagFilter` the list
/// model always had, so nothing here is new behaviour. The column paints
/// its own opaque `sidebar` fill so the split view's vibrancy never shows.
struct NavigationColumn: View {
  let controller: AppController
  let list: MeetingListViewModel
  @Environment(\.openSettings) private var openSettings

  var body: some View {
    VStack(spacing: 0) {
      RecordingControl(controller: controller)
        .padding(.horizontal, Theme.Space.md)
        .padding(.top, Theme.Space.sm)
      ScrollView {
        VStack(alignment: .leading, spacing: Theme.Space.xxs) {
          SectionLabel(text: "Meetings")
            .padding(.horizontal, Theme.Control.rowInset)
            .padding(.bottom, Theme.Space.sm)
          ForEach(MeetingListViewModel.StateFilter.allCases) { filter in
            NavRow(
              id: filter.rawValue, symbol: filter.symbolName, label: filter.title,
              count: list.count(for: filter), isSelected: list.stateFilter == filter
            ) {
              list.stateFilter = filter
            }
          }
          if !list.tags.isEmpty {
            SectionLabel(text: "Tags")
              .padding(.horizontal, Theme.Control.rowInset)
              .padding(.top, Theme.Space.xl)
              .padding(.bottom, Theme.Space.sm)
            ForEach(list.tags, id: \.self) { tag in
              NavRow(
                id: "tag-\(tag)", symbol: "tag", label: "#\(tag)",
                isSelected: list.tagFilter == tag
              ) {
                // Selecting the selected tag clears the filter.
                list.tagFilter = list.tagFilter == tag ? nil : tag
              }
            }
          }
        }
        .padding(.horizontal, Theme.Space.md)
        .padding(.top, Theme.Space.xl)
        .padding(.bottom, Theme.Space.lg)
      }
      Rectangle()
        .fill(Color.stenoBorder)
        .frame(height: Theme.Space.hairline)
      NavRow(id: "settings", symbol: "gearshape", label: "Settings", isSelected: false) {
        openSettings()
      }
      .padding(.horizontal, Theme.Space.md)
      .padding(.vertical, Theme.Space.sm)
    }
    .frame(maxWidth: .infinity, maxHeight: .infinity)
    .background(Color.stenoSidebar)
  }
}
