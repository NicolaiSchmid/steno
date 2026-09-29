import StenoCore
import XCTest

@MainActor
final class MeetingListViewModelTests: XCTestCase {
  func testFiltersByStateAndTagAndKeepsSelection() async throws {
    let environment = try await TestSupport.environment()
    var failed = SampleData.meeting(state: .failed(reason: "boom"))
    failed.id = UUID()
    failed.title = "Failed one"
    failed.tags = ["ops"]
    failed.startedAt = SampleData.startedAt.addingTimeInterval(-3600)
    try await environment.store.save(failed)

    let model = MeetingListViewModel(store: environment.store, clock: ManualClock())
    let observing = Task { await model.observe() }
    defer { observing.cancel() }
    await TestSupport.waitUntil("two meetings") { model.meetings.count == 2 }
    XCTAssertEqual(model.meetings.first?.id, SampleData.meetingID, "newest first")
    XCTAssertEqual(model.tags, ["ops", "q4", "strategie"])

    model.selection = SampleData.meetingID
    model.stateFilter = .failed
    XCTAssertEqual(model.meetings.map(\.id), [failed.id])
    XCTAssertEqual(model.selection, SampleData.meetingID, "a filter never drops the selection")

    model.stateFilter = .all
    model.tagFilter = "q4"
    XCTAssertEqual(model.meetings.map(\.id), [SampleData.meetingID])
    model.tagFilter = nil
    XCTAssertEqual(model.meetings.count, 2)

    // A store update keeps the selection; a vanished meeting clears it.
    try await environment.store.update(meetingID: SampleData.meetingID, now: TestSupport.now) {
      $0.title = "Renamed"
    }
    await TestSupport.waitUntil("rename observed") {
      model.all.first { $0.id == SampleData.meetingID }?.title == "Renamed"
    }
    XCTAssertEqual(model.selection, SampleData.meetingID)
  }

  /// Delete goes through core: the meeting and its asset go, the selection
  /// clears with the list, and a meeting still processing is refused with
  /// the store's reason.
  func testDeleteRemovesTheMeetingAndRefusesABusyOne() async throws {
    let environment = try await TestSupport.environment()
    var processing = SampleData.meeting(state: .processing)
    processing.id = UUID()
    processing.title = "Still processing"
    try await environment.store.save(processing)
    let model = MeetingListViewModel(store: environment.store, clock: ManualClock())
    let observing = Task { await model.observe() }
    defer { observing.cancel() }
    await TestSupport.waitUntil("two meetings") { model.meetings.count == 2 }

    XCTAssertTrue(MeetingListViewModel.canDelete(SampleData.meeting(state: .ready)))
    XCTAssertTrue(MeetingListViewModel.canDelete(SampleData.meeting(state: .failed(reason: "x"))))
    XCTAssertFalse(MeetingListViewModel.canDelete(processing))
    XCTAssertFalse(MeetingListViewModel.canDelete(SampleData.meeting(state: .recording)))

    await model.delete(processing.id)
    XCTAssertEqual(model.error?.hasPrefix("Meeting could not be deleted:"), true, model.error ?? "")
    XCTAssertEqual(model.meetings.count, 2, "a busy meeting stays")

    model.selection = SampleData.meetingID
    model.pendingDeletion = model.all.first { $0.id == SampleData.meetingID }
    await model.delete(SampleData.meetingID)
    XCTAssertNil(model.error)
    XCTAssertNil(model.pendingDeletion)
    await TestSupport.waitUntil("the meeting left the list") { model.meetings.count == 1 }
    XCTAssertNil(model.selection, "the deleted selection clears")
    let gone = try await environment.store.meeting(id: SampleData.meetingID)
    XCTAssertNil(gone)
    let asset = try await environment.store.asset(meetingID: SampleData.meetingID)
    XCTAssertNil(asset)
    let people = try await environment.store.persons()
    XCTAssertEqual(people.count, 2, "people stay")
  }

  /// Three meetings around the fixture (2026-09-24 09:00 UTC), observed by
  /// a model on the UTC calendar so the midnight boundary is where the test
  /// puts it: one late the same day, one just before the midnight before,
  /// one two days earlier, failed and tagged.
  private struct ThreeDays {
    let environment: AppEnvironment
    let model: MeetingListViewModel
    let observing: Task<Void, Never>
    let late: Meeting
    let eve: Meeting
    let old: Meeting
    let utc: Calendar
  }

  private func seedThreeDays() async throws -> ThreeDays {
    let environment = try await TestSupport.environment()
    var utc = Calendar(identifier: .gregorian)
    utc.timeZone = TimeZone(identifier: "UTC")!
    var late = SampleData.meeting()
    late.id = UUID()
    late.title = "Late the same day"
    late.startedAt = SampleData.startedAt.addingTimeInterval(14.5 * 3_600)  // 23:30 UTC
    var eve = SampleData.meeting(state: .processing)
    eve.id = UUID()
    eve.title = "Just before midnight"
    // 23:50 UTC the evening before.
    eve.startedAt = SampleData.startedAt.addingTimeInterval(-(9 * 3_600 + 10 * 60))
    var old = SampleData.meeting(state: .failed(reason: "boom"))
    old.id = UUID()
    old.title = "Two days ago"
    old.tags = ["ops"]
    old.startedAt = SampleData.startedAt.addingTimeInterval(-2 * 86_400)
    for meeting in [late, eve, old] { try await environment.store.save(meeting) }

    let model = MeetingListViewModel(store: environment.store, clock: ManualClock(), calendar: utc)
    let observing = Task { await model.observe() }
    await TestSupport.waitUntil("four meetings") { model.meetings.count == 4 }
    return ThreeDays(
      environment: environment, model: model, observing: observing, late: late, eve: eve,
      old: old, utc: utc)
  }

  /// Cards are cut at midnight in the model's calendar, newest first.
  func testGroupsByDayAtMidnightInTheModelsCalendar() async throws {
    let seeded = try await seedThreeDays()
    defer { seeded.observing.cancel() }
    let model = seeded.model

    XCTAssertEqual(model.dayGroups.count, 3)
    XCTAssertEqual(model.dayGroups[0].meetings.map(\.id), [seeded.late.id, SampleData.meetingID])
    XCTAssertEqual(model.dayGroups[1].meetings.map(\.id), [seeded.eve.id])
    XCTAssertEqual(model.dayGroups[2].meetings.map(\.id), [seeded.old.id])
    XCTAssertEqual(model.dayGroups[0].day, seeded.utc.startOfDay(for: SampleData.startedAt))
    XCTAssertEqual(model.dayGroups[1].day, model.dayGroups[0].day.addingTimeInterval(-86_400))
  }

  /// The nav column's counts read every meeting regardless of the tag
  /// filter; the column title follows the tag, then the state filter, and
  /// "Clear filters" resets both.
  func testCountsIgnoreTheTagFilterAndTheTitleFollowsIt() async throws {
    let seeded = try await seedThreeDays()
    defer { seeded.observing.cancel() }
    let model = seeded.model

    XCTAssertEqual(model.count(for: .all), 4)
    XCTAssertEqual(model.count(for: .processing), 1)
    XCTAssertEqual(model.count(for: .ready), 2)
    XCTAssertEqual(model.count(for: .failed), 1)
    model.tagFilter = "ops"
    XCTAssertEqual(model.meetings.map(\.id), [seeded.old.id])
    XCTAssertEqual(model.count(for: .ready), 2, "counts ignore the tag filter")
    XCTAssertEqual(model.title, "#ops")
    model.tagFilter = nil
    XCTAssertEqual(model.title, "Meetings")
    model.stateFilter = .failed
    XCTAssertEqual(model.title, "Failed")
    model.tagFilter = "ops"
    XCTAssertEqual(model.title, "#ops", "the tag wins over the state filter")
    model.clearFilters()
    XCTAssertEqual(model.title, "Meetings")
    XCTAssertEqual(model.meetings.count, 4)
  }

  /// The arrow keys walk the visible entries across the day boundary, stop
  /// at the ends, and land on the first visible entry when the selection is
  /// hidden by a filter.
  func testArrowKeysWalkTheVisibleEntriesAcrossDays() async throws {
    let seeded = try await seedThreeDays()
    defer { seeded.observing.cancel() }
    let model = seeded.model
    let (late, eve, old) = (seeded.late, seeded.eve, seeded.old)

    XCTAssertNil(model.selection)
    model.selectNext()
    XCTAssertEqual(model.selection, late.id, "nothing selected: the first entry")
    model.selectNext()
    XCTAssertEqual(model.selection, SampleData.meetingID)
    model.selectNext()
    XCTAssertEqual(model.selection, eve.id, "next crosses the day boundary")
    model.selectNext()
    XCTAssertEqual(model.selection, old.id)
    model.selectNext()
    XCTAssertEqual(model.selection, old.id, "the last entry stays")
    model.selectPrevious()
    XCTAssertEqual(model.selection, eve.id)
    model.selectPrevious()
    XCTAssertEqual(model.selection, SampleData.meetingID, "previous crosses the day boundary")
    model.selectPrevious()
    model.selectPrevious()
    XCTAssertEqual(model.selection, late.id, "the first entry stays")

    // A selection the filter hides: the next arrow lands on the first visible entry.
    model.stateFilter = .failed
    XCTAssertEqual(model.selection, late.id, "the filter never drops the selection")
    model.selectNext()
    XCTAssertEqual(model.selection, old.id)
  }

  /// A filter that shows nothing leaves a hidden selection alone: the
  /// arrows have nowhere to go and must not clear the detail pane.
  func testArrowKeysKeepTheSelectionWhenNothingIsVisible() async throws {
    let seeded = try await seedThreeDays()
    defer { seeded.observing.cancel() }
    let model = seeded.model

    model.selection = SampleData.meetingID
    model.tagFilter = "nonexistent"
    XCTAssertTrue(model.meetings.isEmpty)
    model.selectNext()
    XCTAssertEqual(model.selection, SampleData.meetingID)
    model.selectPrevious()
    XCTAssertEqual(model.selection, SampleData.meetingID, "an empty list never drops the selection")
  }

  /// `AppController.requestedMeetingID` selects the live meeting right after
  /// a recording starts, and a store emission snapshotted before the insert
  /// can arrive after that. Only a meeting the list had and lost clears the
  /// selection; an update that never had it leaves it for the one that does.
  func testASelectionMadeAheadOfItsRowSurvivesTheNextUpdate() async throws {
    let environment = try await TestSupport.environment()
    let model = MeetingListViewModel(store: environment.store, clock: ManualClock())
    let observing = Task { await model.observe() }
    defer { observing.cancel() }
    await TestSupport.waitUntil("meeting listed") { model.meetings.count == 1 }

    var live = SampleData.meeting(state: .recording)
    live.id = UUID()
    live.title = "Just started"
    live.startedAt = SampleData.startedAt.addingTimeInterval(3_600)
    model.selection = live.id
    XCTAssertNil(model.selectedMeeting, "not stored yet")

    var other = SampleData.meeting()
    other.id = UUID()
    other.title = "Arrives first"
    other.startedAt = SampleData.startedAt.addingTimeInterval(-3_600)
    try await environment.store.save(other)
    await TestSupport.waitUntil("an update without the selected meeting") {
      model.meetings.count == 2
    }
    XCTAssertEqual(model.selection, live.id, "an update that never had the meeting keeps it")

    try await environment.store.save(live)
    await TestSupport.waitUntil("the selected row arrived") { model.selectedMeeting != nil }
    XCTAssertEqual(model.selection, live.id)
    XCTAssertEqual(model.meetings.first?.id, live.id, "newest first")
  }

  /// The entry's preview: the first summary bullet as "lead: text", else
  /// the state's line; a summary without bullets reads as none and the
  /// failed reason is cut at its first line.
  func testPreviewLineReadsTheSummaryOrTheState() {
    XCTAssertEqual(
      SampleData.meeting().previewLine,
      "Fokus: Speaker 1 schlägt vor, 90 Prozent auf den Kern zu setzen.")
    var noSummary = SampleData.meeting()
    noSummary.summary = nil
    XCTAssertEqual(noSummary.previewLine, "No summary")
    var noBullets = SampleData.meeting()
    noBullets.summary = SummaryDocument(
      templateID: SummaryTemplate.defaultID, language: "de",
      sections: [SummarySection(id: "executive-summary", heading: "Executive Summary", bullets: [])]
    )
    XCTAssertEqual(noBullets.previewLine, "No summary", "a summary without bullets reads as none")
    XCTAssertEqual(SampleData.meeting(state: .processing).previewLine, "Processing")
    XCTAssertEqual(SampleData.meeting(state: .queued).previewLine, "Waiting to process")
    XCTAssertEqual(SampleData.meeting(state: .recording).previewLine, "Recording")
    XCTAssertEqual(
      SampleData.meeting(state: .failed(reason: "The LLM endpoint did not answer.\nRetry later."))
        .previewLine,
      "The LLM endpoint did not answer.")
    XCTAssertEqual(SampleData.meeting(state: .failed(reason: " \n")).previewLine, "Failed")
  }

  func testSearchDebouncesOnTheClockAndUsesFTS() async throws {
    let environment = try await TestSupport.environment()
    let clock = ManualClock()
    let model = MeetingListViewModel(store: environment.store, clock: clock)
    let observing = Task { await model.observe() }
    defer { observing.cancel() }
    await TestSupport.waitUntil("meeting listed") { model.meetings.count == 1 }

    model.query = "Bud"
    model.query = "Budget"
    XCTAssertEqual(model.meetings.count, 1, "nothing changes before the debounce")
    _ = await clock.waitForSleepers(1)
    clock.advance(by: MeetingListViewModel.searchDebounce)
    await TestSupport.waitUntil("search applied") { model.searchHits != nil }
    XCTAssertEqual(model.meetings.map(\.id), [SampleData.meetingID])

    model.query = "zzzznothing"
    _ = await clock.waitForSleepers(1)
    clock.advance(by: MeetingListViewModel.searchDebounce)
    await TestSupport.waitUntil("empty result") { model.meetings.isEmpty }

    model.query = ""
    XCTAssertNil(model.searchHits)
    XCTAssertEqual(model.meetings.count, 1)
  }
}
