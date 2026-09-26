import StenoAudio
import StenoCore
import XCTest

@MainActor
final class DetectionTests: XCTestCase {
  func testPromptCountsDownOnTheClockAndTimesOut() async throws {
    let clock = ManualClock()
    let prompt = DetectionPromptViewModel(appName: "FaceTime", clock: clock, timeout: .seconds(3))
    var outcomes: [DetectionPromptViewModel.Outcome] = []
    prompt.onClose = { outcomes.append($0) }
    prompt.begin()
    XCTAssertEqual(prompt.remainingSeconds, 3)
    for expected in [2, 1] {
      _ = await clock.waitForSleepers(1)
      clock.advance(by: .seconds(1))
      await TestSupport.waitUntil("tick to \(expected)") { prompt.remainingSeconds == expected }
    }
    _ = await clock.waitForSleepers(1)
    clock.advance(by: .seconds(1))
    await TestSupport.waitUntil("timed out") { prompt.outcome == .timedOut }
    XCTAssertEqual(outcomes, [.timedOut])
  }

  func testStartAndDismissCloseOnce() async throws {
    let clock = ManualClock()
    let prompt = DetectionPromptViewModel(appName: "Another app", clock: clock)
    var outcomes: [DetectionPromptViewModel.Outcome] = []
    prompt.onClose = { outcomes.append($0) }
    prompt.begin()
    await prompt.start()
    await prompt.dismiss()
    XCTAssertEqual(outcomes, [.started])
    XCTAssertEqual(prompt.appName, "Another app")
  }

  func testControllerSuppressesWhileRecordingAndWhenDisabled() async throws {
    let clock = ManualClock()
    let environment = try await TestSupport.environment(clock: clock, seed: false)
    let controller = DetectionController(environment: environment)
    controller.appName = { $0 ?? "?" }
    var starts = 0
    controller.startRecording = { starts += 1 }

    await controller.handle(.microphoneOpened(bundleID: "com.zoom.xos", pid: 42))
    XCTAssertNil(controller.prompt, "disabled: no prompt")

    await controller.setEnabled(true)
    await controller.recordingDidChange(true)
    await controller.handle(.microphoneOpened(bundleID: "com.zoom.xos", pid: 42))
    XCTAssertNil(controller.prompt, "recording: no prompt")

    await controller.recordingDidChange(false)
    await controller.handle(.microphoneOpened(bundleID: "com.zoom.xos", pid: 42))
    let prompt = try XCTUnwrap(controller.prompt)
    XCTAssertEqual(prompt.appName, "com.zoom.xos")

    await controller.handle(.microphoneOpened(bundleID: "us.zoom.other", pid: 43))
    XCTAssertTrue(controller.prompt === prompt, "one prompt at a time")

    await prompt.start()
    XCTAssertNil(controller.prompt)
    XCTAssertEqual(starts, 1)

    await controller.handle(.microphoneOpened(bundleID: "com.zoom.xos", pid: 42))
    XCTAssertNotNil(controller.prompt)
    await controller.handle(.microphoneReleased)
    XCTAssertNil(controller.prompt, "released microphone dismisses")
    XCTAssertEqual(starts, 1)

    await controller.handle(.microphoneOpened(bundleID: "com.zoom.xos", pid: 42))
    await controller.setEnabled(false)
    XCTAssertNil(controller.prompt, "disabling dismisses")
    await controller.stop()
  }

  func testRecordingKeepsTheDetectorRunningAndDismissesThePrompt() async throws {
    let clock = ManualClock()
    let environment = try await TestSupport.environment(clock: clock, seed: false)
    let controller = DetectionController(environment: environment)
    controller.appName = { $0 ?? "?" }
    await controller.applySettings()
    XCTAssertTrue(controller.enabled, "the default setting is on")
    var running = await environment.detector.isRunning
    XCTAssertTrue(running)
    await controller.handle(.microphoneOpened(bundleID: "us.zoom.xos", pid: 42))
    XCTAssertNotNil(controller.prompt)

    await controller.recordingDidChange(true)
    XCTAssertNil(controller.prompt, "a recording starting takes the prompt down")
    running = await environment.detector.isRunning
    XCTAssertTrue(running, "the detector is never stopped for Steno's own recording")
    await controller.recordingDidChange(false)
    running = await environment.detector.isRunning
    XCTAssertTrue(running)
    await controller.stop()
  }

  /// The user records the call the prompt announced, then presses Stop while
  /// the other app still holds the microphone: nothing new happened, so no
  /// prompt. (A detector restarted at Stop would report that microphone as
  /// freshly opened after its debounce.)
  func testStopDuringAStillOpenMicrophoneDoesNotRePrompt() async throws {
    let clock = ManualClock()
    let activity = FakeProcessAudioActivity()
    let environment = try await TestSupport.environment(
      clock: clock, seed: false, processActivity: activity)
    let controller = DetectionController(environment: environment)
    controller.appName = { $0 ?? "?" }
    controller.startRecording = { [weak controller] in
      await controller?.recordingDidChange(true)
    }
    await controller.applySettings()
    await TestSupport.waitUntil("poll armed") { clock.pendingSleepers == 1 }

    // t = 0.5: Zoom opens the microphone; the debounce fires at t = 2.5,
    // between the polls (see `step`).
    await step(clock, seconds: 0.5)
    activity.set([Self.zoom])
    await TestSupport.waitUntil("debounce armed") { clock.pendingSleepers == 2 }
    await step(clock, seconds: 2)
    await TestSupport.waitUntil("Zoom's microphone prompts once") { controller.prompt != nil }
    let prompt = try XCTUnwrap(controller.prompt)
    await prompt.start()
    XCTAssertNil(controller.prompt)
    XCTAssertTrue(controller.isRecording)

    // Stop while Zoom still holds the input, then let five polls and more
    // than a full debounce pass.
    await controller.recordingDidChange(false)
    await step(clock, seconds: 5)
    XCTAssertNil(controller.prompt, "the microphone was open all along; no second prompt")

    // Releasing and reopening it is a new call and prompts again.
    activity.set([])
    await TestSupport.waitUntil("release debounce armed") { clock.pendingSleepers == 2 }
    await step(clock, seconds: 2)
    activity.set([Self.zoom])
    await TestSupport.waitUntil("debounce armed again") { clock.pendingSleepers == 2 }
    await step(clock, seconds: 2)
    await TestSupport.waitUntil("a microphone opened anew prompts again") {
      controller.prompt != nil
    }
    await controller.stop()
  }

  /// Moves the clock forward `seconds` in quarter-second steps, letting
  /// every woken task run between steps. The detector's poll wakes on whole
  /// seconds; the tests arm the debounce and the prompt at half-second
  /// offsets, so no two sleepers ever share a deadline and each `advance`
  /// wakes at most one task. `ManualClock.advance` resumes sleepers with
  /// equal deadlines in whichever order the runtime picks, and on Swift 6.0
  /// (hosted macos-15) the poll's evaluate and the debounce's elapse racing
  /// at the same instant left the prompt unset (run 36204744594); on 6.4
  /// (Forge) the same test passed.
  private func step(_ clock: ManualClock, seconds: Double) async {
    let quarters = Int((seconds * 4).rounded())
    for _ in 0..<quarters {
      _ = await clock.waitForSleepers(1)
      clock.advance(by: .milliseconds(250))
      await TestSupport.settle()
      try? await Task.sleep(for: .milliseconds(15))
    }
  }

  private static let zoom = ProcessAudioActivity(
    pid: 4242, bundleID: "us.zoom.xos", isRunningInput: true)

  func testDetectorEventsReachTheControllerAfterTheDebounce() async throws {
    let clock = ManualClock()
    let activity = FakeProcessAudioActivity()
    let environment = try await TestSupport.environment(
      clock: clock, seed: false, processActivity: activity)
    let controller = DetectionController(environment: environment)
    controller.appName = { $0 ?? "?" }
    await controller.applySettings()
    // The detector polls every second on the clock: t = 1.0.
    await TestSupport.waitUntil("poll armed") { clock.pendingSleepers == 1 }

    // t = 0.5: Zoom opens the microphone; the detector arms its 2 s debounce
    // for t = 2.5 and reports nothing inside it.
    await step(clock, seconds: 0.5)
    activity.set([Self.zoom])
    await TestSupport.waitUntil("debounce armed") { clock.pendingSleepers == 2 }
    await step(clock, seconds: 1.5)  // t = 2.0: two polls have run
    XCTAssertNil(controller.prompt, "nothing before the debounce elapses")
    await step(clock, seconds: 0.5)  // t = 2.5: the debounce fires alone
    await TestSupport.waitUntil("prompt after the debounce") { controller.prompt != nil }
    let prompt = try XCTUnwrap(controller.prompt)
    XCTAssertEqual(prompt.appName, "us.zoom.xos")
    XCTAssertEqual(prompt.remainingSeconds, 60)

    // t = 2.75: Zoom releases the microphone; the release debounce (t = 4.75)
    // sits between the prompt's ticks (3.5, 4.5) and the polls.
    await step(clock, seconds: 0.25)
    activity.set([])
    await TestSupport.waitUntil("release debounce armed") { clock.pendingSleepers == 3 }
    await step(clock, seconds: 1.5)  // t = 4.25: still up, one tick down
    XCTAssertTrue(controller.prompt === prompt, "the prompt stays until the release debounce")
    XCTAssertEqual(prompt.remainingSeconds, 59)
    await step(clock, seconds: 0.5)  // t = 4.75
    await TestSupport.waitUntil("prompt dismissed") { controller.prompt == nil }

    // A microphone opened and released within the debounce never prompts.
    activity.set([Self.zoom])
    await TestSupport.waitUntil("debounce armed again") { clock.pendingSleepers == 2 }
    activity.set([])
    await TestSupport.waitUntil("debounce forgotten") { clock.pendingSleepers == 1 }
    await step(clock, seconds: 5)
    XCTAssertNil(controller.prompt, "a blip shorter than the debounce is not a call")
    await controller.stop()
  }

  func testSettingsChangesFollowThroughToTheDetector() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let controller = DetectionController(environment: environment)
    await controller.applySettings()
    XCTAssertTrue(controller.enabled)

    try await environment.updateSettings { $0.meetingDetectionEnabled = false }
    await TestSupport.waitUntil("disabled through Settings") { !controller.enabled }
    var running = await environment.detector.isRunning
    XCTAssertFalse(running, "the detector stops when detection is switched off")
    await controller.handle(.microphoneOpened(bundleID: "us.zoom.xos", pid: 1))
    XCTAssertNil(controller.prompt)

    try await environment.updateSettings { $0.meetingDetectionEnabled = true }
    await TestSupport.waitUntil("re-enabled through Settings") { controller.enabled }
    running = await environment.detector.isRunning
    XCTAssertTrue(running)
    await controller.stop()
    running = await environment.detector.isRunning
    XCTAssertFalse(running, "stop() ends the detector for shutdown")
  }
}
