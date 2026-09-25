import StenoCore
import XCTest

/// The menu bar's presentation: the queue from `observeMeetings()` joined
/// with progress events, the recent list, and the login item toggle.
@MainActor
final class MenuBarViewModelTests: XCTestCase {
  private var observing: [Task<Void, Never>] = []

  override func tearDown() {
    for task in observing { task.cancel() }
    observing = []
  }

  /// A model with both observations running, as `AppController.launch()`
  /// runs them.
  private func makeModel(_ environment: AppEnvironment) -> MenuBarViewModel {
    let model = MenuBarViewModel(environment: environment)
    observing.append(Task { await model.observe() })
    observing.append(Task { await model.observeProgress() })
    return model
  }

  func testQueueOrdersByStartAndFollowsProgress() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = makeModel(environment)
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
    XCTAssertEqual(model.queue.first?.fraction, 0)

    await environment.events.post(.progress(meetingID: earlier.id, stage: .summarize))
    await TestSupport.waitUntil("progress reached the queue") {
      model.queue.first?.stage == .summarize
    }
    XCTAssertEqual(model.queue.first?.fraction, PipelineStage.summarize.fraction)

    try await environment.store.setState(.ready, meetingID: earlier.id, now: TestSupport.now)
    await TestSupport.waitUntil("finished meeting left the queue") { model.queue.count == 1 }
    XCTAssertEqual(model.recent.map(\.id), [earlier.id])
  }

  func testObservationEndsWhenCancelled() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let model = makeModel(environment)
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
    let model = makeModel(environment)
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
    let model = makeModel(environment)
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
    XCTAssertEqual(model.queue.map(\.fraction), [0, 0], "no stage yet")

    // The chip each state renders as, in the queue, the recent list and the
    // detail header.
    let chips = states.map { StatusChip($0).text }
    XCTAssertEqual(chips, ["Recording", "Queued", "Processing", "Ready", "Failed"])
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
    XCTAssertEqual(LanguageTag("de").localizedName(in: Locale(identifier: "en_US")), "German")
  }
}
