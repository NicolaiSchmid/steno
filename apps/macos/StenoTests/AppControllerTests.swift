import StenoAudio
import StenoCore
import XCTest

/// `AppController.launch()` and the wiring between the recorder, the
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

  /// The one meeting the recorder wrote, after it stopped.
  private func recordedMeeting(in environment: AppEnvironment) async throws -> Meeting {
    let meetings = try await environment.store.meetings()
    XCTAssertEqual(meetings.count, 1)
    let meeting = try XCTUnwrap(meetings.first)
    XCTAssertNotEqual(meeting.state, .recording, "the recording was stopped and handed over")
    return meeting
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

  /// The sweep runs on `retentionApplied`, posted after the retention stage
  /// wrote `expiresAt`. The `.ready` row change comes earlier (persist runs
  /// before deliver and retention), so it must not be the trigger: at that
  /// point no asset is expired yet and the files would stay until the next
  /// launch.
  func testRetentionAppliedTriggersTheSweepAndReadyAloneDoesNot() async throws {
    // No seed: the launch sweep has nothing to do, so the files below can
    // only go once an event triggers a sweep.
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

    try await environment.store.setState(
      .ready, meetingID: processing.meeting.id, now: TestSupport.now)
    await TestSupport.waitUntil("the meeting left the queue") {
      !controller.menuBar.queue.contains { $0.id == processing.meeting.id }
    }
    try await Task.sleep(for: .milliseconds(100))
    for url in processing.files {
      XCTAssertTrue(
        FileManager.default.fileExists(atPath: url.path),
        "\(url.lastPathComponent): `.ready` is not the sweep trigger")
    }

    await environment.events.post(.retentionApplied(meetingID: processing.meeting.id))
    await TestSupport.waitUntil("sweep after retention was applied") {
      processing.files.allSatisfy { !FileManager.default.fileExists(atPath: $0.path) }
    }
    XCTAssertTrue(FileManager.default.fileExists(atPath: processing.unrelated.path))
    await controller.shutdown()
  }

  /// "Delete after processing" end to end: a recording stopped with that
  /// retention loses its master and sidecars once the pipeline is done,
  /// without a relaunch.
  func testDeleteAfterProcessingRemovesTheAudioOnceProcessed() async throws {
    let environment = try await TestSupport.environment(seed: false)
    try await environment.updateSettings { $0.defaultRetention = .deleteAfterProcessing }
    let controller = try makeController(environment)
    await controller.launch()
    await controller.recorder.start(mode: .inPerson)
    await controller.recorder.stop()
    let meeting = try await recordedMeeting(in: environment)
    let assetOptional = try await environment.store.asset(meetingID: meeting.id)
    let asset = try XCTUnwrap(assetOptional)
    XCTAssertTrue(FileManager.default.fileExists(atPath: asset.url.path), "master written")

    await environment.pipeline.waitUntilIdle()
    await TestSupport.waitUntil("audio swept after processing") {
      !FileManager.default.fileExists(atPath: asset.url.path)
    }
    let stored = try await environment.store.meeting(id: meeting.id)
    XCTAssertEqual(stored?.state, .ready, "the transcript and summary stay")
    await controller.shutdown()
  }

  /// Quit (or a crash) while meetings were queued or processing: launch
  /// processes them again instead of leaving them in the queue with no
  /// button to reach them. A meeting without an asset row cannot be
  /// processed and is marked failed.
  func testLaunchResumesQueuedAndProcessingMeetings() async throws {
    let environment = try await TestSupport.environment(seed: false)
    // A real recording, so the asset's files exist for the pipeline.
    let recorder = RecordingController(environment: environment)
    await recorder.start(mode: .inPerson)
    await recorder.stop()
    let meeting = try await recordedMeeting(in: environment)
    await environment.pipeline.waitUntilIdle()
    // Pretend the last process died mid-way.
    try await environment.store.setState(.processing, meetingID: meeting.id, now: TestSupport.now)
    var orphan = SampleData.meeting(state: .queued)
    orphan.id = UUID()
    orphan.title = "Queued without an asset"
    try await environment.store.save(orphan)

    let controller = try makeController(environment)
    await controller.launch()
    await environment.pipeline.waitUntilIdle()
    let resumed = try await environment.store.meeting(id: meeting.id)
    XCTAssertEqual(resumed?.state, .ready, "processed again from decode")
    let failed = try await environment.store.meeting(id: orphan.id)
    guard case .failed(let reason)? = failed?.state else {
      return XCTFail("expected .failed, got \(String(describing: failed?.state))")
    }
    XCTAssertTrue(reason.contains("asset is missing"), reason)
    XCTAssertTrue(environment.startupWarnings.isEmpty, "\(environment.startupWarnings)")
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

  func testDetectionPromptStartsACallRecordingWhileTheDetectorRunsOn() async throws {
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
    guard case .recording = controller.recorder.recording else {
      return XCTFail("the prompt's Start should record, got \(controller.recorder.recording)")
    }
    running = await environment.detector.isRunning
    XCTAssertTrue(running, "the detector keeps its view of the open microphone")
    let meetings = try await environment.store.meetings()
    XCTAssertEqual(meetings.map(\.source), [.macCall])

    await controller.detection.handle(.microphoneOpened(bundleID: "us.zoom.xos", pid: 8))
    XCTAssertNil(controller.detection.prompt, "no prompt while recording")

    await controller.recorder.stop()
    running = await environment.detector.isRunning
    XCTAssertTrue(running, "still running after the recording")
    let meeting = try await recordedMeeting(in: environment)
    await environment.pipeline.waitUntilIdle()
    let stored = try await environment.store.meeting(id: meeting.id)
    XCTAssertEqual(stored?.state, .ready)
    await controller.shutdown()
  }

  func testShutdownStopsTheRecordingAndTheDetector() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let controller = try makeController(environment)
    await controller.launch()
    await controller.recorder.start(mode: .inPerson)
    XCTAssertTrue(controller.recorder.isRecording)

    await controller.shutdown()
    XCTAssertEqual(controller.recorder.recording, .idle)
    let meeting = try await recordedMeeting(in: environment)
    XCTAssertFalse(meeting.state.isFailed, "quitting keeps the recording")
    let running = await environment.detector.isRunning
    XCTAssertFalse(running)
    await environment.pipeline.waitUntilIdle()
  }

  /// Quit during the start window (calendar lookup, row write, `session
  /// .start`): the capture is allowed to come up and is then stopped and
  /// enqueued, instead of the process exiting with the writer threads live.
  func testShutdownWhileStartingFinishesTheCapture() async throws {
    let gate = Gate()
    let environment = try await TestSupport.environment(
      seed: false, calendar: GatedCalendar(gate: gate))
    let controller = try makeController(environment)
    await controller.launch()

    let starting = Task { await controller.recorder.start(mode: .call) }
    await TestSupport.waitUntil("the start is waiting on the calendar") {
      controller.recorder.recording == .starting
    }
    let shutdown = Task { await controller.shutdown() }
    await TestSupport.settle()
    await gate.open()
    await starting.value
    await shutdown.value

    XCTAssertEqual(controller.recorder.recording, .idle, "quit stopped the capture it waited for")
    let meeting = try await recordedMeeting(in: environment)
    XCTAssertFalse(meeting.state.isFailed, String(describing: meeting.state))
    await environment.pipeline.waitUntilIdle()
    let stored = try await environment.store.meeting(id: meeting.id)
    XCTAssertEqual(stored?.state, .ready, "the recording was enqueued and processed")
  }

  /// `shutdown()` ends the controller's observations: a store change after
  /// it no longer reaches the menu bar model.
  func testShutdownCancelsTheObservations() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let controller = try makeController(environment)
    await controller.launch()
    var meeting = SampleData.meeting(state: .queued)
    meeting.id = UUID()
    try await environment.store.save(meeting)
    await TestSupport.waitUntil("queued meeting listed") { controller.menuBar.queue.count == 1 }

    await controller.shutdown()
    try await environment.store.setState(.ready, meetingID: meeting.id, now: TestSupport.now)
    await TestSupport.settle()
    try await Task.sleep(for: .milliseconds(100))
    XCTAssertEqual(controller.menuBar.queue.count, 1, "no update after shutdown")
  }
}
