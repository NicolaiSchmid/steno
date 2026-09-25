import Foundation
import StenoCore

/// The sidebar: every meeting from `observeMeetings()`, filtered by state,
/// tag and an FTS query over `MeetingStore.search`. The selection survives
/// list updates as long as the meeting exists.
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

  private(set) var all: [Meeting] = []
  private(set) var meetings: [Meeting] = []
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
  private var searchTask: Task<Void, Never>?
  static let searchDebounce: Duration = .milliseconds(200)

  init(store: MeetingStore, clock: any Clock<Duration>) {
    self.store = store
    self.clock = clock
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

  private func apply() {
    var filtered = all.filter { stateFilter.matches($0) }
    if let tagFilter { filtered = filtered.filter { $0.tags.contains(tagFilter) } }
    if let searchHits { filtered = filtered.filter { searchHits.contains($0.id) } }
    meetings = filtered
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

  // MARK: - Actions

  // Deleting a meeting needs `MeetingStore.delete(meetingID:)` in StenoCore
  // (a follow-up named in the PR); the app owns no SQL.

  /// The meeting's recording folder, for Finder.
  func revealAudio(_ id: UUID) async -> URL? {
    guard let asset = try? await store.asset(meetingID: id) else { return nil }
    return asset.url.deletingLastPathComponent()
  }
}
