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

    activity.set([ProcessAudioActivity(pid: 4242, bundleID: "us.zoom.xos", isRunningInput: true)])
    await TestSupport.waitUntil("debounce armed") { clock.pendingSleepers == 2 }
    for _ in 0..<2 {
      clock.advance(by: .seconds(1))
      await TestSupport.waitUntil("sleepers re-armed") { clock.pendingSleepers >= 1 }
    }
    let prompt = try XCTUnwrap(controller.prompt, "Zoom's microphone prompts once")
    await prompt.start()
    XCTAssertNil(controller.prompt)
    XCTAssertTrue(controller.isRecording)

    // Stop while Zoom still holds the input, then let several polls and a
    // full debounce pass.
    await controller.recordingDidChange(false)
    for _ in 0..<4 {
      _ = await clock.waitForSleepers(1)
      clock.advance(by: .seconds(1))
      await TestSupport.waitUntil("poll re-armed") { clock.pendingSleepers >= 1 }
    }
    XCTAssertNil(controller.prompt, "the microphone was open all along; no second prompt")

    // Releasing and reopening it is a new call and prompts again.
    activity.set([])
    await TestSupport.waitUntil("release debounce armed") { clock.pendingSleepers == 2 }
    clock.advance(by: .seconds(2))
    await TestSupport.waitUntil("released") { clock.pendingSleepers == 1 }
    activity.set([ProcessAudioActivity(pid: 4242, bundleID: "us.zoom.xos", isRunningInput: true)])
    await TestSupport.waitUntil("debounce armed again") { clock.pendingSleepers == 2 }
    clock.advance(by: .seconds(2))
    await TestSupport.waitUntil("second call prompts") { controller.prompt != nil }
    await controller.stop()
  }

  func testDetectorEventsReachTheControllerAfterTheDebounce() async throws {
    let clock = ManualClock()
    let activity = FakeProcessAudioActivity()
    let environment = try await TestSupport.environment(
      clock: clock, seed: false, processActivity: activity)
    let controller = DetectionController(environment: environment)
    controller.appName = { $0 ?? "?" }
    await controller.applySettings()
    // The detector polls every second on the clock.
    await TestSupport.waitUntil("poll armed") { clock.pendingSleepers == 1 }

    // Zoom opens the microphone: the detector arms its 2 s debounce.
    activity.set([ProcessAudioActivity(pid: 4242, bundleID: "us.zoom.xos", isRunningInput: true)])
    await TestSupport.waitUntil("debounce armed") { clock.pendingSleepers == 2 }
    clock.advance(by: .seconds(1))
    await TestSupport.waitUntil("poll re-armed") { clock.pendingSleepers == 2 }
    XCTAssertNil(controller.prompt, "nothing before the debounce elapses")
    clock.advance(by: .seconds(1))
    await TestSupport.waitUntil("prompt after the debounce") { controller.prompt != nil }
    XCTAssertEqual(controller.prompt?.appName, "us.zoom.xos")
    XCTAssertEqual(controller.prompt?.remainingSeconds, 60)

    // Zoom releases the microphone: after the debounce the prompt goes away.
    activity.set([])
    // Sleepers: poll, the prompt's countdown tick, the release debounce.
    await TestSupport.waitUntil("release debounce armed") { clock.pendingSleepers == 3 }
    clock.advance(by: .seconds(2))
    await TestSupport.waitUntil("prompt dismissed") { controller.prompt == nil }

    // A microphone opened and released within the debounce never prompts.
    activity.set([ProcessAudioActivity(pid: 4242, bundleID: "us.zoom.xos", isRunningInput: true)])
    await TestSupport.waitUntil("debounce armed again") { clock.pendingSleepers == 2 }
    activity.set([])
    await TestSupport.waitUntil("debounce forgotten") { clock.pendingSleepers == 1 }
    clock.advance(by: .seconds(5))
    await TestSupport.waitUntil("poll re-armed") { clock.pendingSleepers == 1 }
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
