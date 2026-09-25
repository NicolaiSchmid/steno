import StenoAudio
import StenoCore
import XCTest

@MainActor
final class DetectionTests: XCTestCase {
  func testPromptCountsDownOnTheClockAndTimesOut() async throws {
    let clock = ManualClock()
    let prompt = DetectionPromptViewModel(
      trigger: .microphoneOpened(bundleID: "com.apple.FaceTime", appName: "FaceTime"),
      clock: clock, timeout: .seconds(3))
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
    let prompt = DetectionPromptViewModel(
      trigger: .microphoneOpened(bundleID: nil, appName: "Another app"), clock: clock)
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

  func testControllerFollowsTheDetectorEvents() async throws {
    let clock = ManualClock()
    let environment = try await TestSupport.environment(clock: clock, seed: false)
    let controller = DetectionController(environment: environment)
    controller.appName = { $0 ?? "?" }
    await controller.applySettings()
    XCTAssertTrue(controller.enabled, "the default setting is on")
    let running = await environment.detector.isRunning
    XCTAssertTrue(running)
    await controller.recordingDidChange(true)
    let stopped = await environment.detector.isRunning
    XCTAssertFalse(stopped, "the detector is off while Steno records")
    await controller.recordingDidChange(false)
    let restarted = await environment.detector.isRunning
    XCTAssertTrue(restarted)
    await controller.stop()
  }
}
