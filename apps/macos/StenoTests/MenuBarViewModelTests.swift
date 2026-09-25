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
    let meetingID = try XCTUnwrap(model.lastStoppedMeetingID)
    XCTAssertEqual(meetingID, meetings.first?.id)

    await environment.pipeline.waitUntilIdle()
    let storedOptional = try await environment.store.meeting(id: meetingID)
    let stored = try XCTUnwrap(storedOptional)
    XCTAssertEqual(stored.state, .ready, "the synthetic recording runs through the fake pipeline")
    let assetOptional = try await environment.store.asset(meetingID: meetingID)
    let asset = try XCTUnwrap(assetOptional)
    XCTAssertEqual(asset.retention, .keepDays(30), "retention comes from Settings")
    XCTAssertEqual(asset.lanes, [.mic, .system])
  }

  func testInPersonRecordsOneMixedLane() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = MenuBarViewModel(environment: environment)
    await model.start(mode: .inPerson)
    await model.stop()
    let meetingID = try XCTUnwrap(model.lastStoppedMeetingID)
    let assetOptional = try await environment.store.asset(meetingID: meetingID)
    let asset = try XCTUnwrap(assetOptional)
    XCTAssertEqual(asset.lanes, [.mixed])
    let meeting = try await environment.store.meeting(id: meetingID)
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
    let meetingOptional = try await environment.store.meetings().first
    let meeting = try XCTUnwrap(meetingOptional)
    XCTAssertEqual(meeting.title, "Roadmap sync")
    XCTAssertEqual(meeting.calendarEventID, "event-42")
    let participants = try await environment.store.participants(meetingID: meeting.id)
    XCTAssertEqual(participants.map(\.displayName), ["Jérôme"], "the owner is not an attendee row")
    XCTAssertEqual(participants.first?.role, .them)
    XCTAssertEqual(participants.first?.email, "jerome@example.com")
    await model.stop()
  }

  /// Retention is read when the recording stops, so a setting changed during
  /// the meeting applies to that meeting.
  func testRetentionChangedDuringTheRecordingAppliesToIt() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = MenuBarViewModel(environment: environment)
    await model.start(mode: .call)
    try await environment.updateSettings { $0.defaultRetention = .deleteAfterProcessing }
    await model.stop()
    let meetingID = try XCTUnwrap(model.lastStoppedMeetingID)
    let assetOptional = try await environment.store.asset(meetingID: meetingID)
    let asset = try XCTUnwrap(assetOptional)
    XCTAssertEqual(asset.retention, .deleteAfterProcessing)
    await environment.pipeline.waitUntilIdle()
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

  // MARK: - Failure paths of the state machine

  func testDeviceLossStopsAndEnqueuesThePartialRecording() async throws {
    let environment = try await TestSupport.environment(
      seed: false, makeCaptureSession: TestSupport.deviceLosingCaptureSession(after: 0.5))
    let model = MenuBarViewModel(environment: environment)
    var transitions: [Bool] = []
    model.recordingDidChange = { transitions.append($0) }

    await model.start(mode: .call)
    await TestSupport.waitUntil("device loss ended the recording") { model.recording == .idle }
    let meetingID = try XCTUnwrap(model.lastStoppedMeetingID, "the partial recording was enqueued")
    XCTAssertEqual(model.lastError?.hasPrefix("Recording failed:"), true, model.lastError ?? "")
    XCTAssertEqual(
      model.lastWarning, "An audio device disappeared; the partial recording was kept.")
    XCTAssertEqual(transitions, [true, false], "the detector was paused and resumed once")
    XCTAssertNil(model.levels)

    await environment.pipeline.waitUntilIdle()
    let storedOptional = try await environment.store.meeting(id: meetingID)
    let stored = try XCTUnwrap(storedOptional)
    XCTAssertEqual(stored.state, .ready, "the partial recording ran through the pipeline")
    XCTAssertGreaterThan(stored.duration, 0)
    XCTAssertLessThan(stored.duration, 5, "only the audio before the loss")
    let assetOptional = try await environment.store.asset(meetingID: meetingID)
    let asset = try XCTUnwrap(assetOptional)
    XCTAssertEqual(asset.retention, .keepDays(30))
    XCTAssertTrue(FileManager.default.fileExists(atPath: asset.url.path), "master kept")

    await model.start(mode: .call)
    XCTAssertNotEqual(model.recording, .idle, "a fresh recording can start after the loss")
    await TestSupport.waitUntil("second loss") { model.recording == .idle }
    await environment.pipeline.waitUntilIdle()
  }

  func testAFailingSessionFactoryLeavesNoMeetingRow() async throws {
    let environment = try await TestSupport.environment(
      seed: false,
      makeCaptureSession: { _ in throw CaptureError.invalidState("no input device") })
    let model = MenuBarViewModel(environment: environment)
    var transitions: [Bool] = []
    model.recordingDidChange = { transitions.append($0) }
    await model.start(mode: .call)
    XCTAssertEqual(model.recording, .idle)
    XCTAssertEqual(
      model.lastError?.hasPrefix("Recording could not start:"), true, model.lastError ?? "")
    XCTAssertFalse(transitions.contains(true), "the detector is never paused for a non-start")
    let meetings = try await environment.store.meetings()
    XCTAssertTrue(meetings.isEmpty, "no phantom row when no session could be made")
    XCTAssertNil(model.lastStoppedMeetingID)
  }

  func testAFailingStartMarksTheWrittenRowFailed() async throws {
    let environment = try await TestSupport.environment(
      seed: false,
      makeCaptureSession: { configuration in
        try CaptureSession(configuration: configuration, backend: RefusingBackend())
      })
    let model = MenuBarViewModel(environment: environment)
    await model.start(mode: .inPerson)
    XCTAssertEqual(model.recording, .idle)
    let meetings = try await environment.store.meetings()
    XCTAssertEqual(meetings.count, 1, "the row was written before the backend refused")
    guard case .failed(let reason)? = meetings.first?.state else {
      return XCTFail("expected .failed, got \(String(describing: meetings.first?.state))")
    }
    XCTAssertTrue(reason.contains("Recording could not start"), reason)
    XCTAssertTrue(reason.contains("refused"), reason)
    XCTAssertNil(model.lastStoppedMeetingID, "nothing was enqueued")
  }

  func testToggleRecordingStartsThenStops() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = MenuBarViewModel(environment: environment)
    await model.toggleRecording()
    guard case .recording = model.recording else {
      return XCTFail("toggle from idle starts, got \(model.recording)")
    }
    XCTAssertNotNil(model.elapsed)
    await model.toggleRecording()
    XCTAssertEqual(model.recording, .idle)
    XCTAssertNil(model.elapsed)
    XCTAssertNotNil(model.lastStoppedMeetingID, "toggle from recording stops and enqueues")
    let meetings = try await environment.store.meetings()
    XCTAssertEqual(meetings.first?.source, .macCall, "the shortcut records a call")
    await environment.pipeline.waitUntilIdle()
  }

  func testStopWhileIdleIsIgnored() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = MenuBarViewModel(environment: environment)
    await model.stop()
    XCTAssertEqual(model.recording, .idle)
    XCTAssertNil(model.lastError)
    XCTAssertNil(model.lastStoppedMeetingID)
  }

  // MARK: - Every meeting state in the menu bar

  func testQueueAndRecentPartitionEveryMeetingState() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = MenuBarViewModel(environment: environment)
    let states: [MeetingState] = [.recording, .queued, .processing, .ready, .failed(reason: "x")]
    var ids: [MeetingState.Kind: UUID] = [:]
    for (offset, state) in states.enumerated() {
      var meeting = SampleData.meeting(state: state)
      meeting.id = UUID()
      meeting.title = state.kind.rawValue
      meeting.startedAt = TestSupport.now.addingTimeInterval(TimeInterval(offset) * 60)
      ids[state.kind] = meeting.id
      try await environment.store.save(meeting)
    }
    await TestSupport.waitUntil("queue and recent filled") {
      model.queue.count == 2 && model.recent.count == 2
    }
    XCTAssertEqual(model.queue.map(\.id), [ids[.queued], ids[.processing]], "queue by start")
    XCTAssertEqual(model.recent.map(\.id), [ids[.failed], ids[.ready]], "recent newest first")
    let listed = Set(model.queue.map(\.id) + model.recent.map(\.id))
    let recordingID = try XCTUnwrap(ids[.recording])
    XCTAssertFalse(listed.contains(recordingID), "a live recording is neither queued nor recent")
    XCTAssertEqual(model.queue.map(\.fraction), [0, 0], "no stage yet")

    // The chip each state renders as, in the queue, the recent list and the
    // detail header.
    let chips = states.map { StatusChip($0).text }
    XCTAssertEqual(chips, ["Recording", "Queued", "Processing", "Ready", "Failed"])
  }

  func testRecordingStateLabels() {
    XCTAssertEqual(MenuBarViewModel.RecordingState.idle.label, "Not recording")
    XCTAssertEqual(MenuBarViewModel.RecordingState.starting.label, "Starting…")
    XCTAssertEqual(
      MenuBarViewModel.RecordingState.recording(since: TestSupport.now).label, "Recording")
    XCTAssertEqual(MenuBarViewModel.RecordingState.stopping.label, "Finishing…")
  }
}

/// A backend whose `start` refuses, after the meeting row has been written.
private struct RefusingBackend: CaptureBackend {
  func start(lanes: [AudioLane], inputDeviceUID: String?, sink: LaneFrameSink) throws
    -> CaptureStream
  {
    throw CaptureError.invalidState("the device refused")
  }

  func stop() {}
}
