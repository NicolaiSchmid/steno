import StenoAudio
import StenoCore
import StenoSpeech
import XCTest

/// The recorder's state machine over the synthetic capture backend and the
/// fake pipeline: start, stop, enqueue, the calendar, in-person, and the
/// failure paths. Results are read from the store, never from the model.
@MainActor
final class RecordingControllerTests: XCTestCase {
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
    let stopped = try await TestSupport.stoppedMeeting(in: environment)
    XCTAssertEqual(stopped.id, meetings.first?.id)

    await environment.pipeline.waitUntilIdle()
    let storedOptional = try await environment.store.meeting(id: stopped.id)
    let stored = try XCTUnwrap(storedOptional)
    XCTAssertEqual(stored.state, .ready, "the synthetic recording runs through the fake pipeline")
    let assetOptional = try await environment.store.asset(meetingID: stopped.id)
    let asset = try XCTUnwrap(assetOptional)
    XCTAssertEqual(asset.retention, .keepForever, "retention comes from Settings")
    XCTAssertEqual(asset.lanes, [.mic, .system])
  }

  func testInPersonRecordsOneMixedLane() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let recorder = RecordingController(environment: environment)
    await recorder.start(mode: .inPerson)
    await recorder.stop()
    let meeting = try await TestSupport.stoppedMeeting(in: environment)
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
    let meeting = try await TestSupport.stoppedMeeting(in: environment)
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

  /// Without a calendar event the title is core's default for the source.
  func testDefaultTitleComesFromCore() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let recorder = RecordingController(environment: environment)
    await recorder.start(mode: .inPerson)
    let meetings = try await environment.store.meetings()
    XCTAssertEqual(
      meetings.first?.title,
      LocalRecordingIntake.defaultTitle(source: .macInPerson, startedAt: TestSupport.now))
    XCTAssertEqual(meetings.first?.title.hasPrefix("Meeting "), true)
    await recorder.stop()
    await environment.pipeline.waitUntilIdle()
  }

  // MARK: - Warm-up

  /// With both models on disk a recording start loads the engines through
  /// `AppController`'s hook, so by the time the run prepares them again the
  /// load is a no-op; the capture lost nothing to it.
  func testStartWarmsBothEnginesWhenTheirModelsAreInstalled() async throws {
    let engine = FakeSpeechEngine()
    let diarizer = FakeDiarizer()
    let environment = try await TestSupport.environment(
      seed: false, makeSpeechEngine: { engine }, makeDiarizer: { diarizer })
    for try await _ in await environment.models.ensure(.parakeetV3) {}
    for try await _ in await environment.models.ensure(.offlineDiarizer) {}
    XCTAssertTrue(environment.models.isInstalled(.parakeetV3))
    XCTAssertTrue(environment.models.isInstalled(.offlineDiarizer))
    let controller = AppController(environment: environment)
    let recorder = controller.recorder

    await recorder.start(mode: .call)
    await TestSupport.waitUntil("both engines warmed during the recording") {
      let enginePreparations = await engine.preparations.count
      let diarizerPreparations = await diarizer.preparations.count
      return enginePreparations == 1 && diarizerPreparations == 1
    }
    let transcribedWhileWarm = await engine.transcriptions.count
    XCTAssertEqual(transcribedWhileWarm, 0, "the warm-up came before any transcription")

    await recorder.stop()
    await environment.pipeline.waitUntilIdle()
    let enginePreparations = await engine.preparations.count
    let diarizerPreparations = await diarizer.preparations.count
    XCTAssertEqual(enginePreparations, 2, "the warm-up and the run's own prepare")
    XCTAssertEqual(diarizerPreparations, 2, "the warm-up and the run's own prepare")
    let transcriptions = await engine.transcriptions.count
    XCTAssertEqual(transcriptions, 2, "both lanes of the call")
    let statistics = try XCTUnwrap(recorder.lastStatistics)
    for lane in [AudioLane.mic, .system] {
      XCTAssertEqual(statistics.droppedFrames[lane] ?? 0, 0, "\(lane)")
    }
    let meeting = try await TestSupport.stoppedMeeting(in: environment)
    let stored = try await environment.store.meeting(id: meeting.id)
    XCTAssertEqual(stored?.state, .ready)
    await controller.shutdown()
  }

  /// Without the models on disk nothing is prepared during the recording,
  /// because a `prepare()` may download; the run prepares once after `stop()`.
  func testStartDoesNotWarmWhenModelsAreAbsent() async throws {
    let engine = FakeSpeechEngine()
    let diarizer = FakeDiarizer()
    let environment = try await TestSupport.environment(
      seed: false, makeSpeechEngine: { engine }, makeDiarizer: { diarizer })
    XCTAssertFalse(environment.models.isInstalled(.parakeetV3))
    XCTAssertFalse(environment.models.isInstalled(.offlineDiarizer))
    let controller = AppController(environment: environment)
    let recorder = controller.recorder

    await recorder.start(mode: .call)
    // After a settle a warm-up scheduled a little later would still pass
    // this; the `== 1` after the run is the proof.
    await TestSupport.settle()
    let enginePreparationsWhileRecording = await engine.preparations.count
    let diarizerPreparationsWhileRecording = await diarizer.preparations.count
    XCTAssertEqual(enginePreparationsWhileRecording, 0)
    XCTAssertEqual(diarizerPreparationsWhileRecording, 0)

    await recorder.stop()
    await environment.pipeline.waitUntilIdle()
    let enginePreparations = await engine.preparations.count
    let diarizerPreparations = await diarizer.preparations.count
    XCTAssertEqual(enginePreparations, 1, "only the run prepared; a warm-up would make two")
    XCTAssertEqual(diarizerPreparations, 1, "only the run prepared; a warm-up would make two")
    let meeting = try await TestSupport.stoppedMeeting(in: environment)
    let stored = try await environment.store.meeting(id: meeting.id)
    XCTAssertEqual(stored?.state, .ready)
    await controller.shutdown()
  }

  /// A speech engine setting `SpeechEngineID(settingsValue:)` rejects skips
  /// the warm-up even with both models on disk; the run prepares after
  /// `stop()` as always.
  func testStartDoesNotWarmWhenTheEngineSettingIsUnknown() async throws {
    let engine = FakeSpeechEngine()
    let diarizer = FakeDiarizer()
    let environment = try await TestSupport.environment(
      seed: false, makeSpeechEngine: { engine }, makeDiarizer: { diarizer })
    for try await _ in await environment.models.ensure(.parakeetV3) {}
    for try await _ in await environment.models.ensure(.offlineDiarizer) {}
    try await environment.updateSettings { $0.speechEngineID = "unknown-engine" }
    let controller = AppController(environment: environment)
    let recorder = controller.recorder

    await recorder.start(mode: .inPerson)
    await recorder.stop()
    await environment.pipeline.waitUntilIdle()
    let enginePreparations = await engine.preparations.count
    let diarizerPreparations = await diarizer.preparations.count
    XCTAssertEqual(enginePreparations, 1, "only the run prepared")
    XCTAssertEqual(diarizerPreparations, 1, "only the run prepared")
    let finalState = try await TestSupport.stoppedMeeting(in: environment).state
    XCTAssertEqual(finalState, .ready)
    await controller.shutdown()
  }

  /// A `prepare()` that throws during the recording is swallowed by the
  /// environment's warm-up: no warning, and the run after `stop()` prepares
  /// again and lands the meeting ready.
  func testAFailingWarmUpIsSwallowedAndTheRunPreparesAgain() async throws {
    struct Boom: Error {}
    var engine = FakeSpeechEngine()
    let preparations = engine.preparations
    engine.onPrepare = { if await preparations.count == 1 { throw Boom() } }
    let failingOnce = engine
    let diarizer = FakeDiarizer()
    let environment = try await TestSupport.environment(
      seed: false, makeSpeechEngine: { failingOnce }, makeDiarizer: { diarizer })
    for try await _ in await environment.models.ensure(.parakeetV3) {}
    for try await _ in await environment.models.ensure(.offlineDiarizer) {}
    let controller = AppController(environment: environment)
    let recorder = controller.recorder

    await recorder.start(mode: .call)
    await TestSupport.waitUntil("the warm-up reached the engine") {
      await preparations.count == 1
    }
    let diarizerPreparationsWhileRecording = await diarizer.preparations.count
    XCTAssertEqual(diarizerPreparationsWhileRecording, 0, "the engine threw first")

    await recorder.stop()
    await environment.pipeline.waitUntilIdle()
    let enginePreparations = await preparations.count
    let diarizerPreparations = await diarizer.preparations.count
    XCTAssertEqual(enginePreparations, 2, "the failed warm-up and the run's own prepare")
    XCTAssertEqual(diarizerPreparations, 1, "the run's prepare")
    let finalState = try await TestSupport.stoppedMeeting(in: environment).state
    XCTAssertEqual(finalState, .ready)
    XCTAssertTrue(environment.startupWarnings.isEmpty, "\(environment.startupWarnings)")
    XCTAssertNil(recorder.lastWarning)
    await controller.shutdown()
  }

  func testRecordingStateLabels() {
    XCTAssertEqual(RecordingState.idle.label, "Not recording")
    XCTAssertEqual(RecordingState.starting.label, "Starting…")
    XCTAssertEqual(RecordingState.recording(since: TestSupport.now).label, "Recording")
    XCTAssertEqual(RecordingState.stopping.label, "Finishing…")
  }

  // MARK: - Failure paths of the state machine

  /// Four failed restarts on the manual clock end the recording with
  /// `.deviceLost`; until the fourth the recording continues and says it is
  /// reconnecting, and the device-lost warning appears only at the end.
  func testDeviceLossStopsAndEnqueuesThePartialRecording() async throws {
    let clock = ManualClock()
    let environment = try await TestSupport.environment(
      clock: clock, seed: false,
      makeCaptureSession: TestSupport.deviceLosingCaptureSession(after: 0.5, clock: clock))
    let recorder = RecordingController(environment: environment)
    var transitions: [Bool] = []
    recorder.recordingDidChange = { transitions.append($0) }

    await recorder.start(mode: .call)
    await TestSupport.waitUntil("the change was noticed") {
      recorder.lastWarning == "Audio devices changed. Reconnecting…"
    }
    await TestSupport.advanceThroughTheRestartLadder(clock) {
      guard case .recording = recorder.recording else {
        return XCTFail("a pending restart keeps recording, got \(recorder.recording)")
      }
      XCTAssertNil(recorder.lastError, "no failure before the fourth attempt")
      XCTAssertEqual(recorder.lastWarning, "Audio devices changed. Reconnecting…")
    }
    await TestSupport.waitUntil("device loss ended the recording") { recorder.recording == .idle }
    let meeting = try await TestSupport.stoppedMeeting(in: environment)
    XCTAssertEqual(
      recorder.lastError?.hasPrefix("Recording failed:"), true, recorder.lastError ?? "")
    XCTAssertEqual(
      recorder.lastWarning, "An audio device disappeared; the partial recording was kept.")
    XCTAssertEqual(transitions, [true, false], "the prompt was told once each way")
    XCTAssertNil(recorder.levels)
    XCTAssertEqual(meeting.endReason, .deviceLost)

    await environment.pipeline.waitUntilIdle()
    let storedOptional = try await environment.store.meeting(id: meeting.id)
    let stored = try XCTUnwrap(storedOptional)
    XCTAssertEqual(stored.state, .ready, "the partial recording ran through the pipeline")
    XCTAssertEqual(stored.endReason, .deviceLost, "the reason survives processing")
    XCTAssertGreaterThan(stored.duration, 0)
    XCTAssertLessThan(stored.duration, 5, "only the audio before the loss")
    let assetOptional = try await environment.store.asset(meetingID: meeting.id)
    let asset = try XCTUnwrap(assetOptional)
    XCTAssertEqual(asset.retention, .keepForever)
    XCTAssertTrue(FileManager.default.fileExists(atPath: asset.url.path), "master kept")

    await recorder.start(mode: .call)
    XCTAssertNotEqual(recorder.recording, .idle, "a fresh recording can start after the loss")
    await TestSupport.waitUntil("the second change was noticed") {
      recorder.lastWarning == "Audio devices changed. Reconnecting…"
    }
    await TestSupport.advanceThroughTheRestartLadder(clock)
    await TestSupport.waitUntil("second loss") { recorder.recording == .idle }
    await environment.pipeline.waitUntilIdle()
  }

  /// A change reported inside `backend.start` (the synthetic producer's first
  /// loop iteration) is emitted the moment `session.start` returns. The
  /// recorder subscribes to `notices` before `start`, so the warning still
  /// lands; opened afterwards, the notice would have no continuation to land
  /// in. The first restart fails so the rebuild parks on its backoff sleeper
  /// and nothing else moves until the clock is advanced.
  func testAChangeInTheFirstMillisecondsStillWarns() async throws {
    let clock = ManualClock()
    let environment = try await TestSupport.environment(
      clock: clock, seed: false,
      makeCaptureSession: TestSupport.deviceChangingCaptureSession(
        after: 0, restartsThatFail: 1, clock: clock))
    let recorder = RecordingController(environment: environment)
    await recorder.start(mode: .call)
    await TestSupport.waitUntil("the change was noticed without advancing the clock") {
      recorder.lastWarning == "Audio devices changed. Reconnecting…"
    }
    guard case .recording = recorder.recording else {
      return XCTFail("a pending restart keeps recording, got \(recorder.recording)")
    }

    await TestSupport.waitUntilSleeping(on: clock, count: 1, "the rebuild sleeps on its backoff")
    clock.advance(by: CaptureSession.restartBackoff[0])
    await TestSupport.waitDrivingTheClock(clock, "the resumed warning") {
      recorder.lastWarning == "Audio devices changed. Recording continues."
    }
    await recorder.stop()
    let meeting = try await TestSupport.stoppedMeeting(in: environment)
    XCTAssertEqual(meeting.endReason, .manual)
    await environment.pipeline.waitUntilIdle()
  }

  /// A device that comes back keeps the recording on the same row: the
  /// state never leaves `.recording`, the warning says so, and the meeting
  /// ends with the reason the user gives it.
  func testADeviceChangeKeepsTheRecordingAndWarns() async throws {
    let clock = ManualClock()
    let environment = try await TestSupport.environment(
      clock: clock, seed: false,
      makeCaptureSession: TestSupport.deviceChangingCaptureSession(after: 0.5, clock: clock))
    let recorder = RecordingController(environment: environment)
    await recorder.start(mode: .call)
    await TestSupport.waitDrivingTheClock(clock, "the resumed warning") {
      recorder.lastWarning == "Audio devices changed. Recording continues."
    }
    guard case .recording = recorder.recording else {
      return XCTFail("a survived change keeps recording, got \(recorder.recording)")
    }
    XCTAssertNil(recorder.lastError, recorder.lastError ?? "")

    await recorder.stop()
    XCTAssertEqual(recorder.recording, .idle)
    let meeting = try await TestSupport.stoppedMeeting(in: environment)
    XCTAssertEqual(meeting.endReason, .manual, "a survived change is not a loss")
    XCTAssertNotEqual(
      recorder.lastWarning, "An audio device disappeared; the partial recording was kept.")
    await environment.pipeline.waitUntilIdle()
    let stored = try await environment.store.meeting(id: meeting.id)
    XCTAssertEqual(stored?.state, .ready)
  }

  /// `stop()` stores `.manual`; the reason a caller passes (Quit passes
  /// `.quit`) is stored as given.
  func testStopStoresTheReasonItIsGiven() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let recorder = RecordingController(environment: environment)
    await recorder.start(mode: .inPerson)
    await recorder.stop()
    let first = try await TestSupport.stoppedMeeting(in: environment)
    XCTAssertEqual(first.endReason, .manual)

    await recorder.start(mode: .call)
    await recorder.stop(reason: .quit)
    let meetings = try await environment.store.meetings()
    let second = try XCTUnwrap(meetings.first { $0.id != first.id })
    XCTAssertEqual(second.endReason, .quit)
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
    let meeting = try await TestSupport.stoppedMeeting(in: environment)
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

  /// Nil while idle and while starting (the row is written before the
  /// capture comes up, but nothing is active until it has), the live row's
  /// id while recording, nil again after stop.
  func testActiveMeetingIDFollowsTheRecording() async throws {
    let gate = Gate()
    let environment = try await TestSupport.environment(
      seed: false, calendar: GatedCalendar(gate: gate))
    let recorder = RecordingController(environment: environment)
    XCTAssertNil(recorder.activeMeetingID)

    let starting = Task { await recorder.start(mode: .call) }
    await TestSupport.waitUntil("the start is waiting on the calendar") {
      recorder.recording == .starting
    }
    XCTAssertNil(recorder.activeMeetingID, "nothing is active while starting")
    await gate.open()
    await starting.value
    let meetings = try await environment.store.meetings()
    XCTAssertEqual(meetings.count, 1)
    XCTAssertEqual(recorder.activeMeetingID, meetings.first?.id, "the live row's id")
    await recorder.stop()
    XCTAssertNil(recorder.activeMeetingID, "nothing is active after stop")
    await environment.pipeline.waitUntilIdle()
  }

  /// Only `.denied` required kinds are reported; `.unknown` (macOS has not
  /// asked yet) and optional kinds are not. The report is not a guard:
  /// `start` still runs, so `testAFailingSessionFactoryLeavesNoMeetingRow`
  /// remains the one failure path.
  func testRefreshPermissionsReportsDeniedRequiredKindsOnly() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let permissions = try XCTUnwrap(environment.permissions as? FakePermissions)
    permissions.states[.microphone] = .denied
    permissions.states[.systemAudio] = .unknown
    permissions.states[.calendar] = .denied
    let recorder = RecordingController(environment: environment)
    XCTAssertEqual(recorder.deniedPermissions, [], "nothing reported before a refresh")
    await recorder.refreshPermissions()
    XCTAssertEqual(recorder.deniedPermissions, [.microphone])

    await recorder.start(mode: .call)
    guard case .recording = recorder.recording else {
      return XCTFail("a reported denial does not guard start, got \(recorder.recording)")
    }
    XCTAssertEqual(recorder.deniedPermissions, [.microphone], "start does not touch the report")
    await recorder.stop()

    permissions.states[.microphone] = .granted
    await recorder.refreshPermissions()
    XCTAssertEqual(recorder.deniedPermissions, [], "a fix in System Settings clears the report")

    permissions.states[.microphone] = .denied
    permissions.states[.systemAudio] = .denied
    await recorder.refreshPermissions()
    XCTAssertEqual(
      recorder.deniedPermissions, [.microphone, .systemAudio],
      "reported in `allCases` order; the control joins the messages in this order")
    await environment.pipeline.waitUntilIdle()
  }

  /// After a stop the recorder is idle at once, so a second recording starts
  /// while the first is still queued or processing; each has its own row.
  func testASecondRecordingStartsWhileTheFirstProcesses() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let recorder = RecordingController(environment: environment)
    await recorder.start(mode: .call)
    let first = try XCTUnwrap(recorder.activeMeetingID)
    await recorder.stop()

    await recorder.start(mode: .call)
    guard case .recording = recorder.recording else {
      return XCTFail("expected .recording, got \(recorder.recording)")
    }
    let second = try XCTUnwrap(recorder.activeMeetingID)
    XCTAssertNotEqual(second, first, "a new row for the new recording")
    let meetings = try await environment.store.meetings()
    XCTAssertEqual(meetings.count, 2)
    let firstRow = try XCTUnwrap(meetings.first { $0.id == first })
    XCTAssertNotEqual(firstRow.state, .recording, "the first recording was handed over")
    XCTAssertFalse(firstRow.state.isFailed, String(describing: firstRow.state))
    let secondRow = try XCTUnwrap(meetings.first { $0.id == second })
    XCTAssertEqual(secondRow.state, .recording)

    await recorder.stop()
    await environment.pipeline.waitUntilIdle()
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
