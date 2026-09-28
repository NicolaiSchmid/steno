import StenoCore
import XCTest

/// The shared clock behind the bubble's and the menu bar label's elapsed
/// time: it ticks once a second on the injected clock while the recorder is
/// `.recording` and holds in every other state.
@MainActor
final class RecordingClockTests: XCTestCase {
  func testTicksWhileRecordingAndHoldsOtherwise() async throws {
    let manual = ManualClock()
    var wall = TestSupport.now
    let dates = Dates(initial: wall)
    let clock = RecordingClock(clock: manual, now: { dates.current })
    XCTAssertEqual(clock.now, wall)
    XCTAssertFalse(clock.isTicking)

    clock.update(for: .starting)
    XCTAssertFalse(clock.isTicking, "only `.recording` ticks")

    let since = wall
    clock.update(for: .recording(since: since))
    XCTAssertTrue(clock.isTicking)
    clock.update(for: .recording(since: since))
    let armed = await manual.waitForSleepers(1)
    XCTAssertTrue(armed, "one ticker, armed once")
    XCTAssertEqual(manual.pendingSleepers, 1)

    for second in 1...3 {
      wall = since.addingTimeInterval(TimeInterval(second))
      dates.current = wall
      manual.advance(by: .seconds(1))
      await TestSupport.waitUntil("tick \(second)") { clock.now == wall }
      XCTAssertEqual(clock.elapsed(since: since), TimeInterval(second))
      _ = await manual.waitForSleepers(1)
    }

    clock.update(for: .stopping)
    XCTAssertFalse(clock.isTicking)
    await TestSupport.waitUntil("the sleeper to be cancelled") { manual.pendingSleepers == 0 }
    dates.current = since.addingTimeInterval(60)
    manual.advance(by: .seconds(5))
    await TestSupport.settle()
    XCTAssertEqual(clock.now, wall, "no tick after the recording ended")
    XCTAssertEqual(clock.elapsed(since: since.addingTimeInterval(600)), 0, "never negative")
  }

  /// A mutable date source the clock reads through a `@Sendable` closure.
  private final class Dates: @unchecked Sendable {
    private let lock = NSLock()
    private var value: Date

    init(initial: Date) { value = initial }

    var current: Date {
      get {
        lock.lock()
        defer { lock.unlock() }
        return value
      }
      set {
        lock.lock()
        defer { lock.unlock() }
        value = newValue
      }
    }
  }
}
