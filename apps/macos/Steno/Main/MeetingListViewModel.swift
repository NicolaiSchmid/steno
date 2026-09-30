import Foundation
import StenoCore

/// The list column: every meeting from `observeMeetings()`, filtered by
/// state, tag and an FTS query over `MeetingStore.search`, grouped by
/// calendar day for the cards. The selection survives list updates and
/// clears only when a meeting the list had is gone; `selectNext()` and
/// `selectPrevious()` walk the groups for the arrow keys.
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
      var buckets: [(day: Date, meetings: [Meeting])] = []
      for meeting in meetings {
        let day = calendar.startOfDay(for: meeting.startedAt)
        if let last = buckets.indices.last, buckets[last].day == day {
          buckets[last].meetings.append(meeting)
        } else {
          buckets.append((day: day, meetings: [meeting]))
        }
      }
      return buckets.map { DayGroup(day: $0.day, meetings: $0.meetings) }
    }
  }

  private(set) var all: [Meeting] = []
  private(set) var meetings: [Meeting] = []
  /// `meetings` grouped by day, rebuilt with them.
  private(set) var dayGroups: [DayGroup] = []
  private(set) var searchHits: Set<UUID>?
  private(set) var error: String?
  /// The speakers of every listed meeting and the people they resolve to,
  /// reloaded after each list update, for the rows' speaker chips. The
  /// speaker table is not part of `observeMeetings()`, so a name confirmed
  /// in the detail pane reaches the chips with the next meeting-table write
  /// (the re-export the confirmation schedules), not at once.
  private(set) var speakersByMeeting: [UUID: [Speaker]] = [:]
  private(set) var personsByID: [UUID: Person] = [:]
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
  private var speakersTask: Task<Void, Never>?
  static let searchDebounce: Duration = .milliseconds(200)

  init(store: MeetingStore, clock: any Clock<Duration>, calendar: Calendar = .current) {
    self.store = store
    self.clock = clock
    self.calendar = calendar
  }

  /// Follows `observeMeetings()` until cancelled (the window's `.task`).
  /// Each update names the meetings it removed, so a selection made ahead
  /// of its row (`AppController.requestedMeetingID` right after a recording
  /// starts) survives an emission snapshotted before the insert.
  func observe() async {
    do {
      for try await meetings in store.observeMeetings() {
        let previous = all
        all = meetings.sorted { $0.startedAt > $1.startedAt }
        let current = Set(all.map(\.id))
        apply(removed: previous.filter { !current.contains($0.id) })
        reloadSpeakers()
      }
    } catch {
      self.error = "Meetings could not be loaded: \(error)"
    }
  }

  /// One pass over the listed meetings for their speakers, then the people;
  /// a list update that lands mid-pass drops the pass and starts over, so
  /// the map never mixes two lists.
  private func reloadSpeakers() {
    speakersTask?.cancel()
    let ids = all.map(\.id)
    let store = self.store
    speakersTask = Task { [weak self] in
      var byMeeting: [UUID: [Speaker]] = [:]
      for id in ids {
        guard !Task.isCancelled else { return }
        if let speakers = try? await store.speakers(meetingID: id), !speakers.isEmpty {
          byMeeting[id] = speakers
        }
      }
      let persons = (try? await store.persons()) ?? []
      guard let self, !Task.isCancelled else { return }
      self.speakersByMeeting = byMeeting
      self.personsByID = Dictionary(
        persons.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
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

  /// Rebuilds the visible list; `removed` are the meetings the last store
  /// update dropped, the only thing that clears the selection. A filter
  /// never drops it, and neither does an update that has not yet caught up
  /// with a selection made ahead of its row.
  private func apply(removed: [Meeting] = []) {
    var filtered = all.filter { stateFilter.matches($0) }
    if let tagFilter { filtered = filtered.filter { $0.tags.contains(tagFilter) } }
    if let searchHits { filtered = filtered.filter { searchHits.contains($0.id) } }
    meetings = filtered
    dayGroups = DayGroup.group(filtered, calendar: calendar)
    if let selection, removed.contains(where: { $0.id == selection }) {
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

  /// Down arrow: the next visible entry, across day boundaries; the last
  /// stays.
  func selectNext() { moveSelection(by: 1) }

  /// Up arrow: the previous visible entry, across day boundaries; the first
  /// stays.
  func selectPrevious() { moveSelection(by: -1) }

  /// Both arrows land on the first visible entry when nothing visible is
  /// selected, stay put at either end, and do nothing when the filter shows
  /// no entry, so a hidden selection is never dropped.
  private func moveSelection(by offset: Int) {
    let ids = visibleIDs
    guard let first = ids.first else { return }
    guard let current = selection, let index = ids.firstIndex(of: current) else {
      selection = first
      return
    }
    let target = index + offset
    if ids.indices.contains(target) { selection = ids[target] }
  }

  // MARK: - Actions

  /// The meeting the view is asking the user to confirm deleting.
  var pendingDeletion: Meeting?

  /// The store refuses while the capture writer or the pipeline holds the
  /// meeting's files; the controls say so before the attempt.
  static func canDelete(_ meeting: Meeting) -> Bool {
    switch meeting.state {
    case .recording, .processing: false
    case .queued, .ready, .failed: true
    }
  }

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
