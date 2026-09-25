import StenoAudio
import StenoCore
import XCTest

@MainActor
final class MenuBarViewModelTests: XCTestCase {
  func testStartWritesARecordingMeetingAndStopEnqueuesIt() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = MenuBarViewModel(environment: environment)
    XCTAssertEqual(model.recording, .idle)

    await model.start(mode: .call)
    guard case .recording(let since) = model.recording else {
      return XCTFail("expected .recording, got \(model.recording)")
    }
    XCTAssertEqual(since, TestSupport.now)
    let meetings = try await environment.store.meetings()
    XCTAssertEqual(meetings.count, 1)
    XCTAssertEqual(meetings.first?.state, .recording)
    XCTAssertEqual(meetings.first?.source, .macCall)
    XCTAssertTrue(model.isRecording)

    await model.stop()
    XCTAssertEqual(model.recording, .idle)
    XCTAssertNil(model.lastError, model.lastError ?? "")
    let outcome = try XCTUnwrap(model.lastStop)
    XCTAssertEqual(outcome.meetingID, meetings.first?.id)

    await environment.pipeline.waitUntilIdle()
    let stored = try XCTUnwrap(try await environment.store.meeting(id: outcome.meetingID))
    XCTAssertEqual(stored.state, .ready, "the synthetic recording runs through the fake pipeline")
    let asset = try XCTUnwrap(try await environment.store.asset(meetingID: outcome.meetingID))
    XCTAssertEqual(asset.retention, .keepDays(30), "retention comes from Settings")
    XCTAssertEqual(asset.lanes, [.mic, .system])
  }

  func testInPersonRecordsOneMixedLane() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = MenuBarViewModel(environment: environment)
    await model.start(mode: .inPerson)
    await model.stop()
    let outcome = try XCTUnwrap(model.lastStop)
    let asset = try XCTUnwrap(try await environment.store.asset(meetingID: outcome.meetingID))
    XCTAssertEqual(asset.lanes, [.mixed])
    let meeting = try await environment.store.meeting(id: outcome.meetingID)
    XCTAssertEqual(meeting?.source, .macInPerson)
  }

  func testCalendarEventNamesTheMeetingAndWritesAttendees() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let calendar = try XCTUnwrap(environment.calendar as? FakeCalendar)
    calendar.events = [
      CalendarEvent(
        id: "event-42", title: "Roadmap sync",
        start: TestSupport.now.addingTimeInterval(-300),
        end: TestSupport.now.addingTimeInterval(1800),
        attendees: [
          CalendarAttendee(name: "Jérôme", email: "jerome@example.com", isCurrentUser: false),
          CalendarAttendee(name: "Nicolai", email: "nicolai@example.com", isCurrentUser: true),
        ])
    ]
    let model = MenuBarViewModel(environment: environment)
    await model.start(mode: .call)
    let meeting = try XCTUnwrap(try await environment.store.meetings().first)
    XCTAssertEqual(meeting.title, "Roadmap sync")
    XCTAssertEqual(meeting.calendarEventID, "event-42")
    let participants = try await environment.store.participants(meetingID: meeting.id)
    XCTAssertEqual(participants.map(\.displayName), ["Jérôme"], "the owner is not an attendee row")
    XCTAssertEqual(participants.first?.role, .them)
    XCTAssertEqual(participants.first?.email, "jerome@example.com")
    await model.stop()
  }

  func testStartWhileRecordingIsIgnored() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = MenuBarViewModel(environment: environment)
    await model.start(mode: .call)
    await model.start(mode: .inPerson)
    let meetings = try await environment.store.meetings()
    XCTAssertEqual(meetings.count, 1)
    await model.stop()
  }

  func testQueueOrdersByStartAndFollowsProgress() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = MenuBarViewModel(environment: environment)
    var later = SampleData.meeting(state: .queued)
    later.id = UUID()
    later.startedAt = TestSupport.now.addingTimeInterval(600)
    var earlier = SampleData.meeting(state: .processing)
    earlier.id = UUID()
    earlier.startedAt = TestSupport.now
    try await environment.store.save(later)
    try await environment.store.save(earlier)
    await TestSupport.waitUntil("two queue items") { model.queue.count == 2 }
    XCTAssertEqual(model.queue.map(\.id), [earlier.id, later.id])
    XCTAssertEqual(model.queue.first?.fraction, 0)

    await environment.events.post(.progress(meetingID: earlier.id, stage: .summarize))
    await TestSupport.waitUntil("progress reached the queue") {
      model.queue.first?.stage == .summarize
    }
    XCTAssertEqual(model.queue.first?.fraction, PipelineStage.summarize.fraction)

    try await environment.store.setState(.ready, meetingID: earlier.id, now: TestSupport.now)
    await TestSupport.waitUntil("finished meeting left the queue") { model.queue.count == 1 }
    XCTAssertEqual(model.recent.map(\.id), [earlier.id])
  }

  func testLaunchAtLoginRoundTrip() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let loginItem = try XCTUnwrap(environment.loginItem as? FakeLoginItem)
    let model = MenuBarViewModel(environment: environment)
    XCTAssertEqual(model.launchAtLogin, .notRegistered)
    await model.setLaunchAtLogin(true)
    XCTAssertEqual(model.launchAtLogin, .enabled)
    XCTAssertEqual(loginItem.changes, [true])
    let settings = try await environment.settings.load()
    XCTAssertTrue(settings.launchAtLogin)
    await model.setLaunchAtLogin(false)
    XCTAssertEqual(model.launchAtLogin, .notRegistered)
  }

  func testDefaultTitleNamesTheKind() {
    let title = MenuBarViewModel.defaultTitle(mode: .call, startedAt: TestSupport.now)
    XCTAssertTrue(title.hasPrefix("Call "), title)
    XCTAssertTrue(
      MenuBarViewModel.defaultTitle(mode: .inPerson, startedAt: TestSupport.now)
        .hasPrefix("Meeting "))
  }
}
