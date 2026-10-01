import StenoCore
import XCTest

/// The menu bar's presentation: the queue from `observeMeetings()`, the
/// progress model each queue row reads, the recent list, and the login item
/// toggle.
@MainActor
final class MenuBarViewModelTests: XCTestCase {
  private var observing: [Task<Void, Never>] = []

  /// A nonisolated override under Swift 6.0: hop to the main actor for the
  /// isolated state.
  override func tearDown() async throws {
    await MainActor.run {
      for task in observing { task.cancel() }
      observing = []
    }
  }

  /// The menu bar model observing the meeting list, and the progress model
  /// the queue rows read, fed from the bus and the list as
  /// `AppController.launch()` feeds it: subscribed before returning, so an
  /// event posted right after is seen.
  private func makeModel(_ environment: AppEnvironment) async -> (
    menuBar: MenuBarViewModel, progress: ProcessingProgressModel
  ) {
    let model = MenuBarViewModel(environment: environment)
    let progress = ProcessingProgressModel()
    let events = await environment.events.subscribe()
    observing.append(Task { await model.observe() })
    observing.append(
      Task { await progress.observe(events: events, meetings: environment.store.observeMeetings()) }
    )
    return (model, progress)
  }

  func testQueueOrdersByStartAndFollowsProgress() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let (model, progress) = await makeModel(environment)
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
    await TestSupport.waitUntil("both meetings on the progress model") {
      progress.entry(for: earlier.id) != nil && progress.entry(for: later.id) != nil
    }
    let waiting = try XCTUnwrap(progress.entry(for: earlier.id))
    XCTAssertNil(waiting.stage)
    XCTAssertEqual(waiting.fraction, 0)
    XCTAssertEqual(waiting.title, "Waiting to process")
    XCTAssertNil(waiting.estimatedRemaining)

    let summarizing = ProcessingProgress(
      stage: .summarize, fraction: 0.8, nextFraction: 0.95, estimatedRemaining: .seconds(30),
      isEstimateSeeded: false)
    await environment.events.post(.progress(meetingID: earlier.id, progress: summarizing))
    await TestSupport.waitUntil("progress reached the model") {
      progress.entry(for: earlier.id)?.stage == .summarize
    }
    let entry = try XCTUnwrap(progress.entry(for: earlier.id))
    XCTAssertEqual(entry.progress, summarizing)
    XCTAssertEqual(entry.fraction, 0.8)
    XCTAssertEqual(entry.title, "Summarising…")
    XCTAssertEqual(entry.estimatedRemaining, .seconds(30))
    XCTAssertEqual(progress.entry(for: later.id)?.fraction, 0, "the other meeting is untouched")
    XCTAssertEqual(model.queue.map(\.id), [earlier.id, later.id], "the queue itself is the list")

    try await environment.store.setState(.ready, meetingID: earlier.id, now: TestSupport.now)
    await TestSupport.waitUntil("finished meeting left the queue") { model.queue.count == 1 }
    XCTAssertEqual(model.recent.map(\.id), [earlier.id])
    await TestSupport.waitUntil("finished meeting left the progress model") {
      progress.entry(for: earlier.id) == nil
    }
    XCTAssertNotNil(progress.entry(for: later.id))
  }

  func testObservationEndsWhenCancelled() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let (model, _) = await makeModel(environment)
    var meeting = SampleData.meeting(state: .queued)
    meeting.id = UUID()
    try await environment.store.save(meeting)
    await TestSupport.waitUntil("queued meeting listed") { model.queue.count == 1 }

    for task in observing {
      task.cancel()
      _ = await task.value
    }
    try await environment.store.setState(.ready, meetingID: meeting.id, now: TestSupport.now)
    await TestSupport.settle()
    try await Task.sleep(for: .milliseconds(100))
    XCTAssertEqual(model.queue.count, 1, "a cancelled observation stops updating the model")
  }

  func testLaunchAtLoginRoundTrip() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let loginItem = try XCTUnwrap(environment.loginItem as? FakeLoginItem)
    let (model, _) = await makeModel(environment)
    XCTAssertEqual(model.launchAtLogin, .notRegistered)
    XCTAssertFalse(model.launchAtLogin.isOn)
    await model.setLaunchAtLogin(true)
    XCTAssertEqual(model.launchAtLogin, .enabled)
    XCTAssertTrue(model.launchAtLogin.isOn)
    XCTAssertEqual(loginItem.changes, [true])
    let settings = try await environment.settings.load()
    XCTAssertTrue(settings.launchAtLogin)
    await model.setLaunchAtLogin(false)
    XCTAssertEqual(model.launchAtLogin, .notRegistered)
    XCTAssertTrue(LoginItemStatus.requiresApproval.isOn, "registered but awaiting approval is on")
    XCTAssertFalse(LoginItemStatus.notFound.isOn)
  }

  // MARK: - Every meeting state in the menu bar

  func testQueueAndRecentPartitionEveryMeetingState() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let (model, progress) = await makeModel(environment)
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

    // The progress model tracks exactly the queue: an entry without a stage
    // for each queued or processing meeting, none for the others.
    await TestSupport.waitUntil("the queue on the progress model") {
      progress.entries.count == 2
    }
    XCTAssertEqual(Set(progress.entries.keys), Set(model.queue.map(\.id)))
    XCTAssertEqual(
      model.queue.map { progress.entry(for: $0.id)?.fraction }, [0, 0], "no stage yet")
    XCTAssertEqual(
      model.queue.map { progress.entry(for: $0.id)?.title },
      ["Waiting to process", "Waiting to process"])
    let readyID = try XCTUnwrap(ids[.ready])
    XCTAssertNil(progress.entry(for: readyID))

    // A summary re-run posts progress for a `.ready` meeting; it drives
    // nothing in the queue or the model.
    await environment.events.post(
      .progress(
        meetingID: readyID,
        progress: ProcessingProgress(
          stage: .summarize, fraction: 0, nextFraction: 0.5, estimatedRemaining: .seconds(10),
          isEstimateSeeded: true)))
    await TestSupport.settle()
    XCTAssertNil(progress.entry(for: readyID), "a re-run on a ready meeting is not tracked")
    XCTAssertEqual(model.queue.count, 2)

    // The chip each state renders as, in the queue, the recent list and the
    // detail header.
    let chips = states.map { StatusChip($0).text }
    XCTAssertEqual(chips, ["Recording", "Queued", "Processing", "Ready", "Failed"])
    XCTAssertFalse(states.contains { StatusChip($0).style == .neutral }, "state chips are semantic")
  }

  func testLabelsAreWordsNotRawValues() {
    for stage in PipelineStage.allCases {
      XCTAssertNotEqual(stage.label, stage.rawValue, stage.rawValue)
      XCTAssertEqual(stage.label.first?.isUppercase, true, stage.label)
    }
    XCTAssertEqual(PipelineStage.summarize.label, "Summarising")
    XCTAssertEqual(AudioLane.mic.label, "Mic")
    XCTAssertEqual(AudioLane.system.label, "System")
    XCTAssertEqual(AudioLane.mixed.label, "Room")
    XCTAssertEqual(MeetingSource.macInPerson.label, "In person")
    XCTAssertEqual(PermissionKind.microphone.deniedMessage, "Microphone access is denied.")
    XCTAssertEqual(PermissionKind.systemAudio.deniedMessage, "System audio access is denied.")
  }
}
