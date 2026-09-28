import StenoAudio
import StenoCore
import XCTest

/// The recorder's auto-stop policy over the synthetic capture backend and a
/// `ManualClock`: every row of the device-change plan's "recorder during a
/// call" table, one test each. Results are read from the store.
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

  /// Ticks the countdown `seconds` times, each once its sleeper waits on the
  /// clock; the ticker runs on the main actor between advances.
  private func tick(
    _ clock: ManualClock, seconds: Int, file: StaticString = #filePath, line: UInt = #line
  ) async {
    for _ in 0..<seconds {
      let sleeping = await TestSupport.waitForSleepers(
        clock, 1, "the countdown sleeps on the injected clock", file: file, line: line)
      guard sleeping else { return }
      clock.advance(by: .seconds(1))
      await TestSupport.settle()
    }
  }

  private func onlyMeeting(in environment: AppEnvironment) async throws -> Meeting {
    let meetings = try await environment.store.meetings()
    XCTAssertEqual(meetings.count, 1, "one recording, one row")
    return try XCTUnwrap(meetings.first)
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
    XCTAssertEqual(armed.remaining, RecordingController.autoStopGrace)
    XCTAssertEqual(armed.presentation.line, "Zen closed the microphone. Stopping in 1:30.")
    XCTAssertEqual(armed.presentation.fractionRemaining, 1)
    await recorder.microphoneActivity(.released)
    XCTAssertTrue(
      recorder.autoStop?.countdown === armed.countdown, "a second release does not re-arm")

    await tick(clock, seconds: 1)
    XCTAssertEqual(
      recorder.autoStop?.presentation.line, "Zen closed the microphone. Stopping in 1:29.")
    await tick(clock, seconds: 88)
    XCTAssertEqual(recorder.autoStop?.presentation.remainingText, "0:01")
    guard case .recording = recorder.recording else {
      return XCTFail("still recording at 0:01, got \(recorder.recording)")
    }
    await tick(clock, seconds: 1)
    await TestSupport.waitUntil("the grace elapsed and stopped the recording") {
      recorder.recording == .idle
    }
    XCTAssertNil(recorder.autoStop)
    XCTAssertNil(recorder.lastError, recorder.lastError ?? "")
    let meeting = try await onlyMeeting(in: environment)
    XCTAssertEqual(meeting.endReason, .callEnded(appName: "Zen"))
    XCTAssertNotEqual(meeting.state, .recording)
    await environment.pipeline.waitUntilIdle()
    let stored = try await environment.store.meeting(id: meeting.id)
    XCTAssertEqual(stored?.state, .ready, "an auto-stopped recording is processed like any other")
  }

  func testMicrophoneReopenedCancelsTheCountdown() async throws {
    let clock = ManualClock()
    let (environment, recorder) = try await armedRecorder(clock: clock)
    await tick(clock, seconds: 1)
    XCTAssertEqual(recorder.autoStop?.presentation.remainingText, "1:29")
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
      recorder.autoStop?.remaining, RecordingController.autoStopGrace,
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
    let meeting = try await onlyMeeting(in: environment)
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

  func testAnUnnamedAppUsesTheGenericLine() async throws {
    let clock = ManualClock()
    let (environment, recorder) = try await makeRecorder(clock: clock)
    await recorder.start(mode: .call)
    await recorder.microphoneActivity(.opened(appName: nil))
    await recorder.microphoneActivity(.released)
    let armed = try XCTUnwrap(recorder.autoStop)
    XCTAssertNil(armed.appName)
    XCTAssertEqual(
      armed.presentation.line, "The call app closed the microphone. Stopping in 1:30.")
    await recorder.stopNow()
    let meeting = try await onlyMeeting(in: environment)
    XCTAssertEqual(meeting.endReason, .callEnded(appName: nil))
    await environment.pipeline.waitUntilIdle()
  }

  func testStopNowUsesCallEnded() async throws {
    let clock = ManualClock()
    let (environment, recorder) = try await armedRecorder(clock: clock)
    await recorder.stopNow()
    XCTAssertEqual(recorder.recording, .idle)
    XCTAssertNil(recorder.autoStop)
    let meeting = try await onlyMeeting(in: environment)
    XCTAssertEqual(meeting.endReason, .callEnded(appName: "Zen"))

    await recorder.start(mode: .call)
    await recorder.stopNow()
    guard case .recording = recorder.recording else {
      return XCTFail("stopNow without a countdown is a no-op, got \(recorder.recording)")
    }
    await recorder.stop()
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
    let meeting = try await onlyMeeting(in: environment)
    XCTAssertEqual(meeting.endReason, .manual)

    clock.advance(by: RecordingController.autoStopGrace)
    await TestSupport.settle()
    XCTAssertEqual(recorder.recording, .idle)
    let meetings = try await environment.store.meetings()
    XCTAssertEqual(meetings.count, 1, "nothing else happened")
    await environment.pipeline.waitUntilIdle()
  }

  /// The countdown is armed while a restart is pending on the clock; the
  /// rebuild completes under it and leaves it where it was.
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
    await TestSupport.waitForSleepers(clock, 1, "the first restart failed and the rebuild sleeps")

    await recorder.microphoneActivity(.opened(appName: "Zen"))
    await recorder.microphoneActivity(.released)
    XCTAssertEqual(recorder.autoStop?.appName, "Zen")
    await TestSupport.waitForSleepers(clock, 2, "the rebuild's backoff and the countdown's tick")

    clock.advance(by: CaptureSession.restartBackoff[0])
    await TestSupport.waitDrivingTheClock(clock, sleepers: 2, "the resumed warning") {
      recorder.lastWarning == "Audio devices changed. Recording continues."
    }
    guard case .recording = recorder.recording else {
      return XCTFail("the recording survived the change, got \(recorder.recording)")
    }
    XCTAssertEqual(
      recorder.autoStop?.remaining, RecordingController.autoStopGrace,
      "the countdown neither ticked nor reset")

    await tick(clock, seconds: 1)
    XCTAssertEqual(recorder.autoStop?.presentation.remainingText, "1:29", "and keeps running")
    await recorder.stop()
    let meeting = try await onlyMeeting(in: environment)
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

  func testEventsWhileIdleOrStoppingAreIgnored() async throws {
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
