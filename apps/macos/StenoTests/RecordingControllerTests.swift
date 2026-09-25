import StenoAudio
import StenoCore
import XCTest

/// The recorder's state machine over the synthetic capture backend and the
/// fake pipeline: start, stop, enqueue, the calendar, in-person, and the
/// failure paths. Results are read from the store, never from the model.
@MainActor
final class RecordingControllerTests: XCTestCase {
  /// The one meeting the store holds, once the recording has left `.recording`.
  private func stoppedMeeting(in environment: AppEnvironment) async throws -> Meeting {
    let meetings = try await environment.store.meetings()
    XCTAssertEqual(meetings.count, 1, "one recording, one row")
    let meeting = try XCTUnwrap(meetings.first)
    XCTAssertNotEqual(meeting.state, .recording, "stop left the recording state")
    return meeting
  }

  func testStartWritesARecordingMeetingAndStopEnqueuesIt() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let recorder = RecordingController(environment: environment)
    XCTAssertEqual(recorder.recording, .idle)

    await recorder.start(mode: .call)
    guard case .recording(let since) = recorder.recording else {
      return XCTFail("expected .recording, got \(recorder.recording)")
    }
    XCTAssertEqual(since, TestSupport.now)
    let meetings = try await environment.store.meetings()
    XCTAssertEqual(meetings.count, 1)
    XCTAssertEqual(meetings.first?.state, .recording)
    XCTAssertEqual(meetings.first?.source, .macCall)
    XCTAssertTrue(recorder.isRecording)

    await recorder.stop()
    XCTAssertEqual(recorder.recording, .idle)
    XCTAssertNil(recorder.lastError, recorder.lastError ?? "")
    let stopped = try await stoppedMeeting(in: environment)
    XCTAssertEqual(stopped.id, meetings.first?.id)

    await environment.pipeline.waitUntilIdle()
    let storedOptional = try await environment.store.meeting(id: stopped.id)
    let stored = try XCTUnwrap(storedOptional)
    XCTAssertEqual(stored.state, .ready, "the synthetic recording runs through the fake pipeline")
    let assetOptional = try await environment.store.asset(meetingID: stopped.id)
    let asset = try XCTUnwrap(assetOptional)
    XCTAssertEqual(asset.retention, .keepDays(30), "retention comes from Settings")
    XCTAssertEqual(asset.lanes, [.mic, .system])
  }

  func testInPersonRecordsOneMixedLane() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let recorder = RecordingController(environment: environment)
    await recorder.start(mode: .inPerson)
    await recorder.stop()
    let meeting = try await stoppedMeeting(in: environment)
    let assetOptional = try await environment.store.asset(meetingID: meeting.id)
    let asset = try XCTUnwrap(assetOptional)
    XCTAssertEqual(asset.lanes, [.mixed])
    XCTAssertEqual(meeting.source, .macInPerson)
    await environment.pipeline.waitUntilIdle()
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
    let recorder = RecordingController(environment: environment)
    await recorder.start(mode: .call)
    let meetingOptional = try await environment.store.meetings().first
    let meeting = try XCTUnwrap(meetingOptional)
    XCTAssertEqual(meeting.title, "Roadmap sync")
    XCTAssertEqual(meeting.calendarEventID, "event-42")
    let participants = try await environment.store.participants(meetingID: meeting.id)
    XCTAssertEqual(participants.map(\.displayName), ["Jérôme"], "the owner is not an attendee row")
    XCTAssertEqual(participants.first?.role, .them)
    XCTAssertEqual(participants.first?.email, "jerome@example.com")
    await recorder.stop()
    await environment.pipeline.waitUntilIdle()
  }

  /// Retention is read when the recording stops, so a setting changed during
  /// the meeting applies to that meeting.
  func testRetentionChangedDuringTheRecordingAppliesToIt() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let recorder = RecordingController(environment: environment)
    await recorder.start(mode: .call)
    try await environment.updateSettings { $0.defaultRetention = .deleteAfterProcessing }
    await recorder.stop()
    let meeting = try await stoppedMeeting(in: environment)
    let assetOptional = try await environment.store.asset(meetingID: meeting.id)
    let asset = try XCTUnwrap(assetOptional)
    XCTAssertEqual(asset.retention, .deleteAfterProcessing)
    await environment.pipeline.waitUntilIdle()
  }

  func testStartWhileRecordingIsIgnored() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let recorder = RecordingController(environment: environment)
    await recorder.start(mode: .call)
    await recorder.start(mode: .inPerson)
    let meetings = try await environment.store.meetings()
    XCTAssertEqual(meetings.count, 1)
    await recorder.stop()
    await environment.pipeline.waitUntilIdle()
  }

  func testDefaultTitleNamesTheKind() {
    let title = RecordingController.defaultTitle(mode: .call, startedAt: TestSupport.now)
    XCTAssertTrue(title.hasPrefix("Call "), title)
    XCTAssertTrue(
      RecordingController.defaultTitle(mode: .inPerson, startedAt: TestSupport.now)
        .hasPrefix("Meeting "))
  }

  func testRecordingStateLabels() {
    XCTAssertEqual(RecordingState.idle.label, "Not recording")
    XCTAssertEqual(RecordingState.starting.label, "Starting…")
    XCTAssertEqual(RecordingState.recording(since: TestSupport.now).label, "Recording")
    XCTAssertEqual(RecordingState.stopping.label, "Finishing…")
  }

  // MARK: - Failure paths of the state machine

  func testDeviceLossStopsAndEnqueuesThePartialRecording() async throws {
    let environment = try await TestSupport.environment(
      seed: false, makeCaptureSession: TestSupport.deviceLosingCaptureSession(after: 0.5))
    let recorder = RecordingController(environment: environment)
    var transitions: [Bool] = []
    recorder.recordingDidChange = { transitions.append($0) }

    await recorder.start(mode: .call)
    await TestSupport.waitUntil("device loss ended the recording") { recorder.recording == .idle }
    let meeting = try await stoppedMeeting(in: environment)
    XCTAssertEqual(
      recorder.lastError?.hasPrefix("Recording failed:"), true, recorder.lastError ?? "")
    XCTAssertEqual(
      recorder.lastWarning, "An audio device disappeared; the partial recording was kept.")
    XCTAssertEqual(transitions, [true, false], "the prompt was told once each way")
    XCTAssertNil(recorder.levels)

    await environment.pipeline.waitUntilIdle()
    let storedOptional = try await environment.store.meeting(id: meeting.id)
    let stored = try XCTUnwrap(storedOptional)
    XCTAssertEqual(stored.state, .ready, "the partial recording ran through the pipeline")
    XCTAssertGreaterThan(stored.duration, 0)
    XCTAssertLessThan(stored.duration, 5, "only the audio before the loss")
    let assetOptional = try await environment.store.asset(meetingID: meeting.id)
    let asset = try XCTUnwrap(assetOptional)
    XCTAssertEqual(asset.retention, .keepDays(30))
    XCTAssertTrue(FileManager.default.fileExists(atPath: asset.url.path), "master kept")

    await recorder.start(mode: .call)
    XCTAssertNotEqual(recorder.recording, .idle, "a fresh recording can start after the loss")
    await TestSupport.waitUntil("second loss") { recorder.recording == .idle }
    await environment.pipeline.waitUntilIdle()
  }

  func testAFailingSessionFactoryLeavesNoMeetingRow() async throws {
    let environment = try await TestSupport.environment(
      seed: false,
      makeCaptureSession: { _ in throw CaptureError.invalidState("no input device") })
    let recorder = RecordingController(environment: environment)
    var transitions: [Bool] = []
    recorder.recordingDidChange = { transitions.append($0) }
    await recorder.start(mode: .call)
    XCTAssertEqual(recorder.recording, .idle)
    XCTAssertEqual(
      recorder.lastError?.hasPrefix("Recording could not start:"), true, recorder.lastError ?? "")
    XCTAssertFalse(transitions.contains(true), "nothing is told about a non-start")
    let meetings = try await environment.store.meetings()
    XCTAssertTrue(meetings.isEmpty, "no phantom row when no session could be made")
  }

  func testAFailingStartMarksTheWrittenRowFailed() async throws {
    let environment = try await TestSupport.environment(
      seed: false,
      makeCaptureSession: { configuration in
        try CaptureSession(configuration: configuration, backend: RefusingBackend())
      })
    let recorder = RecordingController(environment: environment)
    await recorder.start(mode: .inPerson)
    XCTAssertEqual(recorder.recording, .idle)
    let meetings = try await environment.store.meetings()
    XCTAssertEqual(meetings.count, 1, "the row was written before the backend refused")
    guard case .failed(let reason)? = meetings.first?.state else {
      return XCTFail("expected .failed, got \(String(describing: meetings.first?.state))")
    }
    XCTAssertTrue(reason.contains("Recording could not start"), reason)
    XCTAssertTrue(reason.contains("refused"), reason)
    let asset = try await environment.store.asset(meetingID: meetings[0].id)
    XCTAssertNil(asset, "nothing was enqueued")
  }

  func testToggleRecordingStartsThenStops() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let recorder = RecordingController(environment: environment)
    await recorder.toggleRecording()
    guard case .recording = recorder.recording else {
      return XCTFail("toggle from idle starts, got \(recorder.recording)")
    }
    XCTAssertNotNil(recorder.elapsed)
    await recorder.toggleRecording()
    XCTAssertEqual(recorder.recording, .idle)
    XCTAssertNil(recorder.elapsed)
    let meeting = try await stoppedMeeting(in: environment)
    XCTAssertEqual(meeting.source, .macCall, "the shortcut records a call")
    await environment.pipeline.waitUntilIdle()
  }

  func testStopWhileIdleIsIgnored() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let recorder = RecordingController(environment: environment)
    await recorder.stop()
    XCTAssertEqual(recorder.recording, .idle)
    XCTAssertNil(recorder.lastError)
    let meetings = try await environment.store.meetings()
    XCTAssertTrue(meetings.isEmpty)
  }

  func testProbeResultShowsAsAWarning() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let recorder = RecordingController(environment: environment)
    recorder.noteProbe(granted: true)
    XCTAssertEqual(recorder.lastWarning?.contains("permission granted"), true)
    recorder.noteProbe(granted: false)
    XCTAssertEqual(recorder.lastWarning?.contains("stayed silent"), true)
    recorder.clearMessages()
    XCTAssertNil(recorder.lastWarning)
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
