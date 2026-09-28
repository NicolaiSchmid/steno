import StenoAudio
import StenoCore
import XCTest

/// The recorder's auto-stop policy over the synthetic capture backend and a
/// `ManualClock`: every row of the device-change plan's "recorder during a
/// call" table, one test each. Results are read from the store. The
/// countdown sentence itself is pinned in `RecordingControlPresentationTests`;
/// only the first test here reads it, end to end.
@MainActor
final class AutoStopTests: XCTestCase {
  private func makeRecorder(
    clock: ManualClock, makeCaptureSession: AppEnvironment.MakeCaptureSession? = nil
  ) async throws -> (environment: AppEnvironment, recorder: RecordingController) {
    let environment = try await TestSupport.environment(
      clock: clock, seed: false, makeCaptureSession: makeCaptureSession)
    return (environment, RecordingController(environment: environment))
  }

  /// A `.call` recording in which Zen held and then released the microphone.
  private func armedRecorder(clock: ManualClock) async throws -> (
    environment: AppEnvironment, recorder: RecordingController
  ) {
    let made = try await makeRecorder(clock: clock)
    await made.recorder.start(mode: .call)
    await made.recorder.microphoneActivity(.opened(appName: "Zen"))
    await made.recorder.microphoneActivity(.released)
    XCTAssertNotNil(made.recorder.autoStop, "armed")
    return made
  }

  func testMicrophoneReleaseAfterACallArmsAndStopsAfterTheGrace() async throws {
    let clock = ManualClock()
    let (environment, recorder) = try await makeRecorder(clock: clock)
    await recorder.start(mode: .call)
    await recorder.microphoneActivity(.opened(appName: "Zen"))
    XCTAssertNil(recorder.autoStop, "an open microphone is a call in progress")

    await recorder.microphoneActivity(.released)
    let armed = try XCTUnwrap(recorder.autoStop)
    XCTAssertEqual(armed.appName, "Zen")
    XCTAssertEqual(armed.countdown.remaining, RecordingController.autoStopGrace)
    XCTAssertEqual(armed.presentation.line, "Zen closed the microphone. Stopping in 1:30.")
    XCTAssertEqual(armed.presentation.fractionRemaining, 1)
    await recorder.microphoneActivity(.released)
    XCTAssertTrue(
      recorder.autoStop?.countdown === armed.countdown, "a second release does not re-arm")

    await TestSupport.tick(clock, seconds: 1)
    await TestSupport.waitUntil("one tick") {
      recorder.autoStop?.presentation.remainingText == "1:29"
    }
    await TestSupport.tick(clock, seconds: 88)
    await TestSupport.waitUntil("the last second") {
      recorder.autoStop?.presentation.remainingText == "0:01"
    }
    guard case .recording = recorder.recording else {
      return XCTFail("still recording at 0:01, got \(recorder.recording)")
    }
    await TestSupport.tick(clock, seconds: 1)
    await TestSupport.waitUntil("the grace elapsed and stopped the recording") {
      recorder.recording == .idle
    }
    XCTAssertNil(recorder.autoStop)
    XCTAssertNil(recorder.lastError, recorder.lastError ?? "")
    let meeting = try await TestSupport.stoppedMeeting(in: environment)
    XCTAssertEqual(meeting.endReason, .callEnded(appName: "Zen"))
    await environment.pipeline.waitUntilIdle()
    let stored = try await environment.store.meeting(id: meeting.id)
    XCTAssertEqual(stored?.state, .ready, "an auto-stopped recording is processed like any other")
  }

  func testMicrophoneReopenedCancelsTheCountdown() async throws {
    let clock = ManualClock()
    let (environment, recorder) = try await armedRecorder(clock: clock)
    await TestSupport.tick(clock, seconds: 1)
    await TestSupport.waitUntil("one tick") {
      recorder.autoStop?.presentation.remainingText == "1:29"
    }
    _ = await clock.waitForSleepers(1)

    await recorder.microphoneActivity(.opened(appName: "Zen"))
    XCTAssertNil(recorder.autoStop, "the call is back on")
    XCTAssertEqual(clock.pendingSleepers, 0, "the countdown's sleeper is withdrawn")
    clock.advance(by: RecordingController.autoStopGrace)
    await TestSupport.settle()
    guard case .recording = recorder.recording else {
      return XCTFail("a cancelled countdown never stops, got \(recorder.recording)")
    }

    await recorder.microphoneActivity(.released)
    XCTAssertEqual(
      recorder.autoStop?.countdown.remaining, RecordingController.autoStopGrace,
      "the next release arms a fresh countdown")
    await recorder.stop()
    await environment.pipeline.waitUntilIdle()
  }

  func testKeepRecordingCancelsUntilTheNextCall() async throws {
    let clock = ManualClock()
    let (environment, recorder) = try await armedRecorder(clock: clock)
    _ = await clock.waitForSleepers(1)
    recorder.keepRecording()
    XCTAssertNil(recorder.autoStop)
    XCTAssertEqual(clock.pendingSleepers, 0)

    await recorder.microphoneActivity(.released)
    XCTAssertNil(recorder.autoStop, "another release does not re-arm")
    await recorder.microphoneActivity(.opened(appName: "Meet"))
    // A "Keep recording" click that lands after the row is gone (the call
    // app reopened the microphone) must not wipe the memory of that call.
    recorder.keepRecording()
    await recorder.microphoneActivity(.released)
    XCTAssertEqual(recorder.autoStop?.appName, "Meet", "the next call arms again")
    await recorder.stop()
    XCTAssertNil(recorder.autoStop)
    await environment.pipeline.waitUntilIdle()
  }

  func testInPersonNeverArms() async throws {
    let clock = ManualClock()
    let (environment, recorder) = try await makeRecorder(clock: clock)
    await recorder.start(mode: .inPerson, callApp: "FaceTime")
    await recorder.microphoneActivity(.opened(appName: "FaceTime"))
    await recorder.microphoneActivity(.released)
    XCTAssertNil(recorder.autoStop)
    XCTAssertEqual(clock.pendingSleepers, 0)
    await recorder.stop()
    let meeting = try await TestSupport.stoppedMeeting(in: environment)
    XCTAssertEqual(meeting.endReason, .manual)
    await environment.pipeline.waitUntilIdle()
  }

  func testNoForeignMicrophoneNeverArms() async throws {
    let clock = ManualClock()
    let (environment, recorder) = try await makeRecorder(clock: clock)
    await recorder.start(mode: .call)
    await recorder.microphoneActivity(.released)
    XCTAssertNil(recorder.autoStop, "no call was ever observed")
    await recorder.stop()
    await environment.pipeline.waitUntilIdle()
  }

  /// An `.opened` whose bundle id could not be named arms all the same and
  /// stores a nameless `.callEnded`; the generic line follows from the nil
  /// name (`RecordingControlPresentationTests`).
  func testAnUnnamedAppStoresANamelessCallEnded() async throws {
    let clock = ManualClock()
    let (environment, recorder) = try await makeRecorder(clock: clock)
    await recorder.start(mode: .call)
    await recorder.microphoneActivity(.opened(appName: nil))
    await recorder.microphoneActivity(.released)
    let armed = try XCTUnwrap(recorder.autoStop)
    XCTAssertNil(armed.appName)
    await TestSupport.tick(clock, seconds: 90)
    await TestSupport.waitUntil("the grace elapsed") { recorder.recording == .idle }
    let meeting = try await TestSupport.stoppedMeeting(in: environment)
    XCTAssertEqual(meeting.endReason, .callEnded(appName: nil))
    await environment.pipeline.waitUntilIdle()
  }

  func testManualStopWhileArmedStoresManualAndClearsTheCountdown() async throws {
    let clock = ManualClock()
    let (environment, recorder) = try await armedRecorder(clock: clock)
    _ = await clock.waitForSleepers(1)
    await recorder.stop()
    XCTAssertEqual(recorder.recording, .idle)
    XCTAssertNil(recorder.autoStop)
    XCTAssertEqual(clock.pendingSleepers, 0, "the countdown went with the recording")
    let meeting = try await TestSupport.stoppedMeeting(in: environment)
    XCTAssertEqual(meeting.endReason, .manual)

    clock.advance(by: RecordingController.autoStopGrace)
    await TestSupport.settle()
    XCTAssertEqual(recorder.recording, .idle)
    let meetings = try await environment.store.meetings()
    XCTAssertEqual(meetings.count, 1, "nothing else happened")
    await environment.pipeline.waitUntilIdle()
  }

  /// Quit during the grace stores `.quit`, not the countdown's reason, and
  /// withdraws the sleeper so the elapsed callback cannot fire into a
  /// torn-down app.
  func testQuitWhileArmedStoresQuitAndWithdrawsTheCountdown() async throws {
    let clock = ManualClock()
    let (environment, recorder) = try await armedRecorder(clock: clock)
    _ = await clock.waitForSleepers(1)
    await recorder.stop(reason: .quit)
    XCTAssertEqual(recorder.recording, .idle)
    XCTAssertNil(recorder.autoStop)
    XCTAssertEqual(clock.pendingSleepers, 0, "the countdown went with the recording")
    let meeting = try await TestSupport.stoppedMeeting(in: environment)
    XCTAssertEqual(meeting.endReason, .quit)

    clock.advance(by: RecordingController.autoStopGrace)
    await TestSupport.settle()
    let meetings = try await environment.store.meetings()
    XCTAssertEqual(meetings.count, 1, "nothing else happened")
    await environment.pipeline.waitUntilIdle()
  }

  /// The countdown is armed while a restart is pending on the clock; the
  /// rebuild completes under it and leaves it where it was. Identity is the
  /// observable: the exact `remaining` would couple the test to how far the
  /// rebuild's relay waits advanced the shared clock.
  func testADeviceChangeNoticeLeavesTheCountdownRunning() async throws {
    let clock = ManualClock()
    let (environment, recorder) = try await makeRecorder(
      clock: clock,
      makeCaptureSession: TestSupport.deviceChangingCaptureSession(
        after: 0.5, restartsThatFail: 1, clock: clock))
    await recorder.start(mode: .call)
    await TestSupport.waitUntil("the change was noticed") {
      recorder.lastWarning == "Audio devices changed. Reconnecting…"
    }
    await TestSupport.waitUntilSleeping(
      on: clock, count: 1, "the first restart failed and the rebuild sleeps")

    await recorder.microphoneActivity(.opened(appName: "Zen"))
    await recorder.microphoneActivity(.released)
    let armed = try XCTUnwrap(recorder.autoStop)
    XCTAssertEqual(armed.appName, "Zen")
    await TestSupport.waitUntilSleeping(
      on: clock, count: 2, "the rebuild's backoff and the countdown's tick")

    clock.advance(by: CaptureSession.restartBackoff[0])
    await TestSupport.waitDrivingTheClock(clock, sleepers: 2, "the resumed warning") {
      recorder.lastWarning == "Audio devices changed. Recording continues."
    }
    guard case .recording = recorder.recording else {
      return XCTFail("the recording survived the change, got \(recorder.recording)")
    }
    XCTAssertTrue(
      recorder.autoStop?.countdown === armed.countdown,
      "the countdown was neither reset nor re-armed")
    XCTAssertTrue(armed.countdown.isRunning)
    XCTAssertGreaterThanOrEqual(
      armed.countdown.remaining, RecordingController.autoStopGrace - .seconds(1),
      "at most the one tick the driven clock may have crossed")

    let before = armed.countdown.remaining
    await TestSupport.tick(clock, seconds: 1)
    await TestSupport.waitUntil("and keeps running") {
      armed.countdown.remaining == before - .seconds(1)
    }
    await recorder.stop()
    let meeting = try await TestSupport.stoppedMeeting(in: environment)
    XCTAssertEqual(meeting.endReason, .manual)
    await environment.pipeline.waitUntilIdle()
  }

  func testStopResetsTheForeignMicrophoneMemory() async throws {
    let clock = ManualClock()
    let (environment, recorder) = try await makeRecorder(clock: clock)
    await recorder.start(mode: .call)
    await recorder.microphoneActivity(.opened(appName: "Zen"))
    await recorder.stop()

    await recorder.start(mode: .call)
    await recorder.microphoneActivity(.released)
    XCTAssertNil(recorder.autoStop, "the second recording saw no call of its own")
    await recorder.stop()
    let meetings = try await environment.store.meetings()
    XCTAssertEqual(meetings.map(\.endReason), [.manual, .manual])
    await environment.pipeline.waitUntilIdle()
  }

  func testAPromptStartedRecordingArmsOnReleaseWithoutAnOpenedEvent() async throws {
    let clock = ManualClock()
    let (environment, recorder) = try await makeRecorder(clock: clock)
    await recorder.start(mode: .call, callApp: "Zen")
    await recorder.microphoneActivity(.released)
    XCTAssertEqual(recorder.autoStop?.appName, "Zen")
    recorder.keepRecording()
    await recorder.stop()
    await environment.pipeline.waitUntilIdle()
  }

  /// The detector forwards from the moment the start is announced, so a call
  /// joined while Steno is still starting (settings, calendar, the intake
  /// row, the backend) is remembered and its release arms.
  func testAMicrophoneOpenedWhileStartingIsRemembered() async throws {
    let clock = ManualClock()
    let gate = Gate()
    let environment = try await TestSupport.environment(
      clock: clock, seed: false, calendar: GatedCalendar(gate: gate))
    let recorder = RecordingController(environment: environment)
    let starting = Task { await recorder.start(mode: .call) }
    await TestSupport.waitUntil("the start is waiting on the calendar") {
      recorder.recording == .starting
    }
    await recorder.microphoneActivity(.opened(appName: "Zen"))
    XCTAssertNil(recorder.autoStop)
    await gate.open()
    await starting.value

    await recorder.microphoneActivity(.released)
    XCTAssertEqual(recorder.autoStop?.appName, "Zen", "the call joined during the start window")
    await recorder.stop()
    await environment.pipeline.waitUntilIdle()
  }

  func testEventsWhileIdleAreIgnoredAndNotRemembered() async throws {
    let clock = ManualClock()
    let (environment, recorder) = try await makeRecorder(clock: clock)
    await recorder.microphoneActivity(.opened(appName: "Zen"))
    await recorder.microphoneActivity(.released)
    XCTAssertNil(recorder.autoStop)
    XCTAssertEqual(recorder.recording, .idle)

    await recorder.start(mode: .call)
    await recorder.microphoneActivity(.released)
    XCTAssertNil(recorder.autoStop, "the idle-time `.opened` was not remembered")
    await recorder.stop()
    await environment.pipeline.waitUntilIdle()
  }
}
