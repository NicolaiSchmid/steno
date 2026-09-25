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

  func testSearchDebouncesOnTheClockAndUsesFTS() async throws {
    let environment = try await TestSupport.environment()
    let clock = ManualClock()
    let model = MeetingListViewModel(store: environment.store, clock: clock)
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
