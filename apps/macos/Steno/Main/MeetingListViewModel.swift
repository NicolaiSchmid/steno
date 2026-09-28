import Foundation
import StenoCore

/// The list column: every meeting from `observeMeetings()`, filtered by
/// state, tag and an FTS query over `MeetingStore.search`, grouped by
/// calendar day for the cards. The selection survives list updates as long
/// as the meeting exists; `selectNext()` and `selectPrevious()` walk the
/// groups for the arrow keys.
@MainActor
@Observable
final class MeetingListViewModel {
  enum StateFilter: String, CaseIterable, Identifiable, Sendable {
    case all
    case processing
    case ready
    case failed

    var id: String { rawValue }

    var title: String {
      switch self {
      case .all: "All"
      case .processing: "In progress"
      case .ready: "Ready"
      case .failed: "Failed"
      }
    }

    func matches(_ meeting: Meeting) -> Bool {
      switch self {
      case .all: true
      case .processing:
        meeting.state == .recording || meeting.state == .queued
          || meeting.state == .processing
      case .ready: meeting.state == .ready
      case .failed: meeting.state.isFailed
      }
    }
  }

  /// One card: the meetings that started on `day` (the day's start in the
  /// model's calendar), newest first.
  struct DayGroup: Identifiable, Equatable, Sendable {
    let day: Date
    let meetings: [Meeting]

    var id: Date { day }

    /// Buckets `meetings` (already newest first) by the start of their day
    /// in `calendar`, keeping the order, so the groups run newest first too.
    static func group(_ meetings: [Meeting], calendar: Calendar) -> [DayGroup] {
      var groups: [DayGroup] = []
      for meeting in meetings {
        let day = calendar.startOfDay(for: meeting.startedAt)
        if let last = groups.indices.last, groups[last].day == day {
          groups[last] = DayGroup(day: day, meetings: groups[last].meetings + [meeting])
        } else {
          groups.append(DayGroup(day: day, meetings: [meeting]))
        }
      }
      return groups
    }
  }

  private(set) var all: [Meeting] = []
  private(set) var meetings: [Meeting] = []
  /// `meetings` grouped by day, rebuilt with them.
  private(set) var dayGroups: [DayGroup] = []
  private(set) var searchHits: Set<UUID>?
  private(set) var error: String?
  var query = "" {
    didSet { if query != oldValue { scheduleSearch() } }
  }
  var stateFilter: StateFilter = .all {
    didSet { apply() }
  }
  var tagFilter: String? {
    didSet { apply() }
  }
  var selection: UUID?

  private let store: MeetingStore
  private let clock: any Clock<Duration>
  /// Day boundaries for the cards; the viewer's calendar and time zone.
  let calendar: Calendar
  private var searchTask: Task<Void, Never>?
  static let searchDebounce: Duration = .milliseconds(200)

  init(store: MeetingStore, clock: any Clock<Duration>, calendar: Calendar = .current) {
    self.store = store
    self.clock = clock
    self.calendar = calendar
  }

  /// Follows `observeMeetings()` until cancelled (the window's `.task`).
  func observe() async {
    do {
      for try await meetings in store.observeMeetings() {
        all = meetings.sorted { $0.startedAt > $1.startedAt }
        apply()
      }
    } catch {
      self.error = "Meetings could not be loaded: \(error)"
    }
  }

  var tags: [String] {
    Array(Set(all.flatMap(\.tags))).sorted()
  }

  /// How many of every meeting a filter row would show, before the tag
  /// filter and the query; the nav column's counts.
  func count(for filter: StateFilter) -> Int {
    all.filter { filter.matches($0) }.count
  }

  /// The list column's heading: the tag, else the state filter, else
  /// "Meetings".
  var title: String {
    if let tagFilter { return "#\(tagFilter)" }
    return stateFilter == .all ? "Meetings" : stateFilter.title
  }

  /// Whether the empty list is empty because of the query or the filters,
  /// as opposed to an empty store.
  var isFiltering: Bool {
    stateFilter != .all || tagFilter != nil || !query.trimmingCharacters(in: .whitespaces).isEmpty
  }

  /// The "Clear filters" action of the no-match empty state.
  func clearFilters() {
    query = ""
    stateFilter = .all
    tagFilter = nil
  }

  /// The selected meeting, when it is still stored.
  var selectedMeeting: Meeting? {
    guard let selection else { return nil }
    return all.first { $0.id == selection }
  }

  private func apply() {
    var filtered = all.filter { stateFilter.matches($0) }
    if let tagFilter { filtered = filtered.filter { $0.tags.contains(tagFilter) } }
    if let searchHits { filtered = filtered.filter { searchHits.contains($0.id) } }
    meetings = filtered
    dayGroups = DayGroup.group(filtered, calendar: calendar)
    if let selection, !all.contains(where: { $0.id == selection }) {
      self.selection = nil
    }
  }

  private func scheduleSearch() {
    searchTask?.cancel()
    let trimmed = query.trimmingCharacters(in: .whitespaces)
    guard !trimmed.isEmpty else {
      searchHits = nil
      apply()
      return
    }
    let clock = self.clock
    let store = self.store
    searchTask = Task { [weak self] in
      do {
        try await clock.sleep(for: Self.searchDebounce)
      } catch {
        return
      }
      let hits = try? await store.search(trimmed, limit: 200)
      guard let self, !Task.isCancelled else { return }
      self.searchHits = Set((hits ?? []).map(\.meetingID))
      self.apply()
    }
  }

  // MARK: - Keyboard selection

  /// The visible entries in reading order: the groups newest first, the
  /// meetings inside each newest first.
  private var visibleIDs: [UUID] {
    dayGroups.flatMap { $0.meetings.map(\.id) }
  }

  /// Down arrow: the next visible entry, across day boundaries; the first
  /// when nothing visible is selected; the last stays.
  func selectNext() {
    let ids = visibleIDs
    guard let current = selection, let index = ids.firstIndex(of: current) else {
      selection = ids.first
      return
    }
    if index + 1 < ids.count { selection = ids[index + 1] }
  }

  /// Up arrow: the previous visible entry, across day boundaries; the first
  /// when nothing visible is selected; the first stays.
  func selectPrevious() {
    let ids = visibleIDs
    guard let current = selection, let index = ids.firstIndex(of: current) else {
      selection = ids.first
      return
    }
    if index > 0 { selection = ids[index - 1] }
  }

  // MARK: - Actions

  /// The meeting the view is asking the user to confirm deleting.
  var pendingDeletion: Meeting?

  /// Core's `MeetingStore.delete(meetingID:)`: rows, receipt and the
  /// meeting's files go; a meeting still recording or processing is
  /// refused and the reason shown. A deleted selection clears itself when
  /// the list updates.
  func delete(_ id: UUID) async {
    pendingDeletion = nil
    do {
      try await store.delete(meetingID: id)
      error = nil
    } catch let failure as MeetingStoreError {
      error = "Meeting could not be deleted: \(failure.description)"
    } catch {
      self.error = "Meeting could not be deleted: \(error)"
    }
  }

  /// The meeting's recording folder, for Finder.
  func revealAudio(_ id: UUID) async -> URL? {
    guard let asset = try? await store.asset(meetingID: id) else { return nil }
    return asset.url.deletingLastPathComponent()
  }
}
