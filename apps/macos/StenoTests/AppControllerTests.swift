import StenoAudio
import StenoCore
import XCTest

/// `AppController.launch()` and the wiring between the menu bar, the
/// detection prompt, the retention sweep and pending speaker reviews, all
/// over the preview environment's fakes.
@MainActor
final class AppControllerTests: XCTestCase {
  private var defaultsSuite = ""

  override func setUp() {
    defaultsSuite = "uno.schmid.steno.mac.tests.\(UUID().uuidString)"
  }

  override func tearDown() {
    UserDefaults.standard.removePersistentDomain(forName: defaultsSuite)
  }

  private func makeController(_ environment: AppEnvironment) throws -> AppController {
    let defaults = try XCTUnwrap(UserDefaults(suiteName: defaultsSuite))
    return AppController(environment: environment, defaults: defaults)
  }

  /// A meeting whose audio files live in a fresh temporary folder and whose
  /// retention expired a minute ago; one unrelated file sits beside them.
  private func expiredRecording(in environment: AppEnvironment, state: MeetingState)
    async throws -> (meeting: Meeting, files: [URL], unrelated: URL)
  {
    let folder = try TestSupport.temporaryDirectory("steno-sweep")
    var meeting = SampleData.meeting(state: state)
    meeting.id = UUID()
    meeting.title = "Expired \(state.kind.rawValue)"
    meeting.startedAt = TestSupport.now.addingTimeInterval(-7200)
    var asset = SampleData.audioAsset()
    asset.id = UUID()
    asset.meetingID = meeting.id
    asset.url = folder.appendingPathComponent("master.caf")
    asset.sidecars16k = [
      .mic: folder.appendingPathComponent("mic.wav"),
      .system: folder.appendingPathComponent("system.wav"),
    ]
    asset.mixdownURL = folder.appendingPathComponent("audio.m4a")
    asset.retention = .keepDays(1)
    asset.expiresAt = TestSupport.now.addingTimeInterval(-60)
    let unrelated = folder.appendingPathComponent("notes.txt")
    for url in asset.expirableFiles + [unrelated] {
      try Data("x".utf8).write(to: url)
    }
    try await environment.store.save(meeting, asset: asset)
    return (meeting, asset.expirableFiles, unrelated)
  }

  func testLaunchMarksInterruptedRecordingsFailedAndSweepsExpiredAudio() async throws {
    let environment = try await TestSupport.environment()
    var interrupted = SampleData.meeting(state: .recording)
    interrupted.id = UUID()
    interrupted.title = "Crashed mid-call"
    try await environment.store.save(interrupted)
    let expired = try await expiredRecording(in: environment, state: .ready)
    defer { try? FileManager.default.removeItem(at: expired.unrelated.deletingLastPathComponent()) }
    XCTAssertEqual(expired.files.count, 4, "master, two sidecars, mixdown")

    let controller = try makeController(environment)
    XCTAssertFalse(controller.launched)
    await controller.launch()
    XCTAssertTrue(controller.launched)

    let stored = try await environment.store.meeting(id: interrupted.id)
    guard case .failed(let reason)? = stored?.state else {
      return XCTFail(
        "interrupted recording should be failed, got \(String(describing: stored?.state))")
    }
    XCTAssertTrue(reason.localizedCaseInsensitiveContains("interrupted"), reason)
    let untouched = try await environment.store.meeting(id: SampleData.meetingID)
    XCTAssertEqual(untouched?.state, .ready, "a finished meeting is not touched")

    for url in expired.files {
      XCTAssertFalse(
        FileManager.default.fileExists(atPath: url.path), "\(url.lastPathComponent) swept")
    }
    XCTAssertTrue(
      FileManager.default.fileExists(atPath: expired.unrelated.path), "only app-written files go")
    let assetAfter = try await environment.store.asset(meetingID: expired.meeting.id)
    XCTAssertNil(assetAfter?.expiresAt, "a swept asset loses its expiry so the sweep runs once")
    XCTAssertTrue(environment.startupWarnings.isEmpty, "\(environment.startupWarnings)")

    await controller.launch()
    XCTAssertTrue(controller.launched, "a second launch is a no-op")
    await controller.shutdown()
  }

  func testAMeetingReachingReadyTriggersTheRetentionSweep() async throws {
    // No seed: the launch sweep has nothing to do, so the files below can
    // only go once the meeting's own state change triggers a sweep.
    let environment = try await TestSupport.environment(seed: false)
    let controller = try makeController(environment)
    await controller.launch()

    let processing = try await expiredRecording(in: environment, state: .processing)
    defer {
      try? FileManager.default.removeItem(at: processing.unrelated.deletingLastPathComponent())
    }
    await TestSupport.waitUntil("the processing meeting reached the queue") {
      controller.menuBar.queue.contains { $0.id == processing.meeting.id }
    }
    for url in processing.files {
      XCTAssertTrue(
        FileManager.default.fileExists(atPath: url.path),
        "\(url.lastPathComponent) stays while the meeting is still processing")
    }

    try await environment.store.setState(
      .ready, meetingID: processing.meeting.id, now: TestSupport.now)
    await TestSupport.waitUntil("sweep after the meeting finished") {
      processing.files.allSatisfy { !FileManager.default.fileExists(atPath: $0.path) }
    }
    XCTAssertTrue(FileManager.default.fileExists(atPath: processing.unrelated.path))
    await controller.shutdown()
  }

  func testSpeakersNeedReviewStaysPendingUntilTheReviewCompletes() async throws {
    let environment = try await TestSupport.environment()
    let controller = try makeController(environment)
    await controller.launch()
    XCTAssertTrue(controller.pendingReviews.isEmpty)

    // The bus replays nothing, so the event is posted until the controller's
    // subscription (made inside `launch()`) has seen it.
    await TestSupport.waitUntil("review pending") {
      await environment.events.post(
        .speakersNeedReview(
          meetingID: SampleData.meetingID, speakerIDs: [SampleData.speakerTwoID]))
      return controller.pendingReviews.contains(SampleData.meetingID)
    }
    controller.reviewCompleted(meetingID: SampleData.meetingID)
    XCTAssertFalse(controller.pendingReviews.contains(SampleData.meetingID))

    // A review for a meeting the store does not list is dropped on the next
    // list change, so a badge never points at nothing.
    let ghost = UUID()
    await environment.events.post(.speakersNeedReview(meetingID: ghost, speakerIDs: []))
    await TestSupport.waitUntil("ghost pending") { controller.pendingReviews.contains(ghost) }
    try await environment.store.update(meetingID: SampleData.meetingID, now: TestSupport.now) {
      $0.title = "Renamed"
    }
    await TestSupport.waitUntil("ghost dropped") { !controller.pendingReviews.contains(ghost) }
    await controller.shutdown()
  }

  func testDetectionPromptStartsACallRecordingAndPausesTheDetector() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let controller = try makeController(environment)
    controller.detection.appName = { $0 ?? "?" }
    await controller.launch()
    var running = await environment.detector.isRunning
    XCTAssertTrue(running, "detection is on by default")

    await controller.detection.handle(.microphoneOpened(bundleID: "com.apple.FaceTime", pid: 7))
    let prompt = try XCTUnwrap(controller.detection.prompt)
    XCTAssertEqual(prompt.appName, "com.apple.FaceTime")
    await prompt.start()

    XCTAssertNil(controller.detection.prompt)
    guard case .recording = controller.menuBar.recording else {
      return XCTFail("the prompt's Start should record, got \(controller.menuBar.recording)")
    }
    running = await environment.detector.isRunning
    XCTAssertFalse(running, "the detector never sees Steno's own tap")
    let meetings = try await environment.store.meetings()
    XCTAssertEqual(meetings.map(\.source), [.macCall])

    await controller.detection.handle(.microphoneOpened(bundleID: "us.zoom.xos", pid: 8))
    XCTAssertNil(controller.detection.prompt, "no prompt while recording")

    await controller.menuBar.stop()
    running = await environment.detector.isRunning
    XCTAssertTrue(running, "the detector resumes after the recording")
    let meetingID = try XCTUnwrap(controller.menuBar.lastStoppedMeetingID)
    await environment.pipeline.waitUntilIdle()
    let stored = try await environment.store.meeting(id: meetingID)
    XCTAssertEqual(stored?.state, .ready)
    await controller.shutdown()
  }

  func testShutdownStopsTheRecordingAndTheDetector() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let controller = try makeController(environment)
    await controller.launch()
    await controller.menuBar.start(mode: .inPerson)
    XCTAssertTrue(controller.menuBar.isRecording)

    await controller.shutdown()
    XCTAssertEqual(controller.menuBar.recording, .idle)
    XCTAssertNotNil(controller.menuBar.lastStoppedMeetingID, "quitting keeps the recording")
    let running = await environment.detector.isRunning
    XCTAssertFalse(running)
  }
}
