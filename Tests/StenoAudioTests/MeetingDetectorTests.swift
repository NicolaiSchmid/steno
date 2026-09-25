import Foundation
import StenoCore
import Testing

@testable import StenoAudio

/// `MeetingDetector` on `FakeProcessAudioActivity` and `ManualClock`: no
/// real sleeping anywhere. `waitForSleepers` synchronises with the detector's
/// timers (the poll is one sleeper, an armed debounce a second one).
@Suite(.timeLimit(.minutes(2))) struct MeetingDetectorTests {
  static let faceTime = ProcessAudioActivity(
    pid: 4_242, bundleID: "com.apple.FaceTime", isRunningInput: true)
  static let zoom = ProcessAudioActivity(pid: 5_151, bundleID: "us.zoom.xos", isRunningInput: true)
  static let idleZoom = ProcessAudioActivity(
    pid: 5_151, bundleID: "us.zoom.xos", isRunningInput: false)

  func makeDetector(_ source: FakeProcessAudioActivity, clock: ManualClock) -> MeetingDetector {
    MeetingDetector(source: source, clock: clock, ignoringPIDs: [1])
  }

  /// Yields until the clock has exactly `count` sleepers.
  func waitForSleepers(_ clock: ManualClock, exactly count: Int) async -> Bool {
    for _ in 0..<10_000 {
      if clock.pendingSleepers == count { return true }
      await Task.yield()
    }
    return clock.pendingSleepers == count
  }

  @Test func aFlappingInputYieldsOneOpenedAndOneReleased() async throws {
    let clock = ManualClock()
    let source = FakeProcessAudioActivity([Self.idleZoom])
    let detector = makeDetector(source, clock: clock)
    let events = await detector.events
    try await detector.start()
    #expect(await clock.waitForSleepers(1), "the poll timer is armed")

    source.set([Self.faceTime])
    #expect(await waitForSleepers(clock, exactly: 2), "the open debounce is armed")
    source.set([])
    #expect(await waitForSleepers(clock, exactly: 1), "released within the debounce: disarmed")
    source.set([Self.faceTime, Self.idleZoom])
    #expect(await waitForSleepers(clock, exactly: 2))

    clock.advance(by: .seconds(2))
    var iterator = events.makeAsyncIterator()
    let opened = await iterator.next()
    #expect(opened == .microphoneOpened(bundleID: "com.apple.FaceTime", pid: 4_242))
    #expect(await detector.holder == Self.faceTime)

    // Still open two polls later: nothing new.
    #expect(await clock.waitForSleepers(1))
    clock.advance(by: .seconds(2))
    #expect(await clock.waitForSleepers(1))
    clock.advance(by: .seconds(2))
    #expect(await clock.waitForSleepers(1))

    source.set([Self.idleZoom])
    #expect(await waitForSleepers(clock, exactly: 2), "the release debounce is armed")
    source.set([Self.faceTime])
    #expect(await waitForSleepers(clock, exactly: 1), "a flap back cancels the release")
    source.set([])
    #expect(await waitForSleepers(clock, exactly: 2))
    clock.advance(by: .seconds(2))
    let released = await iterator.next()
    #expect(released == .microphoneReleased)
    #expect(await detector.holder == nil)

    await detector.stop()
    var trailing: [MeetingDetector.Event] = []
    while let event = await iterator.next() { trailing.append(event) }
    #expect(trailing.isEmpty, "exactly one opened and one released: \(trailing)")
  }

  @Test func ownProcessIsIgnored() async throws {
    let clock = ManualClock()
    let own = ProcessAudioActivity(pid: 1, bundleID: "uno.schmid.steno.mac", isRunningInput: true)
    let source = FakeProcessAudioActivity([own])
    let detector = makeDetector(source, clock: clock)
    try await detector.start()
    #expect(await clock.waitForSleepers(1))
    #expect(clock.pendingSleepers == 1, "no debounce for our own microphone use")
    source.set([own, Self.idleZoom])
    #expect(await clock.waitForSleepers(1))
    clock.advance(by: .seconds(4))
    #expect(await clock.waitForSleepers(1))
    #expect(await detector.holder == nil)
    await detector.stop()
  }

  @Test func thePollNoticesWhatTheListenerMissed() async throws {
    let clock = ManualClock()
    let source = FakeProcessAudioActivity()
    let detector = makeDetector(source, clock: clock)
    let events = await detector.events
    try await detector.start()
    #expect(await clock.waitForSleepers(1))
    let before = source.snapshotCount

    source.setSilently([Self.zoom])
    clock.advance(by: .seconds(2))  // the poll fires and arms the debounce
    #expect(await clock.waitForSleepers(2))
    #expect(source.snapshotCount > before)
    clock.advance(by: .seconds(2))
    var iterator = events.makeAsyncIterator()
    #expect(await iterator.next() == .microphoneOpened(bundleID: "us.zoom.xos", pid: 5_151))
    await detector.stop()
  }

  /// The plan's acceptance: a microphone opened right after a poll, with no
  /// HAL listener event, is reported within 3 s (1 s poll plus 2 s debounce).
  /// With the earlier 2 s poll the worst case was 4 s.
  @Test func withoutAListenerEventAnOpenedMicrophoneIsReportedWithinThreeSeconds() async throws {
    let clock = ManualClock()
    let source = FakeProcessAudioActivity()
    let detector = makeDetector(source, clock: clock)
    let events = await detector.events
    try await detector.start()
    #expect(await clock.waitForSleepers(1))
    #expect(await detector.pollInterval == .seconds(1))
    let before = source.snapshotCount

    source.setSilently([Self.zoom])
    clock.advance(by: .seconds(1))
    #expect(await clock.waitForSleepers(2), "the poll fired and armed the debounce")
    #expect(source.snapshotCount > before, "one second after opening, the poll has seen it")
    clock.advance(by: .seconds(2))
    var iterator = events.makeAsyncIterator()
    #expect(await iterator.next() == .microphoneOpened(bundleID: "us.zoom.xos", pid: 5_151))
    #expect(clock.now == ManualClock.Instant(offset: .seconds(3)))
    await detector.stop()
  }

  @Test func anAlreadyOpenMicrophoneIsReportedAfterTheDebounce() async throws {
    let clock = ManualClock()
    let source = FakeProcessAudioActivity([Self.zoom])
    let detector = makeDetector(source, clock: clock)
    let events = await detector.events
    try await detector.start()
    #expect(await clock.waitForSleepers(2), "poll plus debounce")
    clock.advance(by: .seconds(2))
    var iterator = events.makeAsyncIterator()
    #expect(await iterator.next() == .microphoneOpened(bundleID: "us.zoom.xos", pid: 5_151))
    await detector.stop()
    #expect(await !detector.isRunning)
  }

  @Test func aFailingSnapshotFailsStartAndIsToleratedLater() async throws {
    struct Broken: Error {}
    let clock = ManualClock()
    let source = FakeProcessAudioActivity([Self.zoom])
    source.failure = Broken()
    let detector = makeDetector(source, clock: clock)
    await #expect(throws: Broken.self) { try await detector.start() }
    #expect(await !detector.isRunning)

    source.failure = nil
    let events = await detector.events
    try await detector.start()
    #expect(await clock.waitForSleepers(2))
    source.failure = Broken()
    source.set([])
    clock.advance(by: .seconds(2))
    // The failed re-read changed nothing; the debounce still fires on what
    // was last seen.
    var iterator = events.makeAsyncIterator()
    #expect(await iterator.next() == .microphoneOpened(bundleID: "us.zoom.xos", pid: 5_151))
    #expect(await detector.holder == Self.zoom)
    await detector.stop()
  }
}
