import StenoCore
import Synchronization
import XCTest

/// The shared clock behind the bubble's and the menu bar label's elapsed
/// time: it ticks once a second on the injected clock while the recorder is
/// `.recording` and holds in every other state.
@MainActor
final class RecordingClockTests: XCTestCase {
  func testTicksWhileRecordingAndHoldsOtherwise() async throws {
    let manual = ManualClock()
    let since = TestSupport.now
    // The wall clock the `RecordingClock` reads on every tick.
    let wall = Mutex(since)
    let clock = RecordingClock(clock: manual, now: { wall.withLock { $0 } })
    XCTAssertEqual(clock.now, since)
    XCTAssertFalse(clock.isTicking)

    clock.update(for: .starting)
    XCTAssertFalse(clock.isTicking, "only `.recording` ticks")

    clock.update(for: .recording(since: since))
    XCTAssertTrue(clock.isTicking)
    clock.update(for: .recording(since: since))
    let armed = await manual.waitForSleepers(1)
    XCTAssertTrue(armed, "one ticker, armed once")
    XCTAssertEqual(manual.pendingSleepers, 1)

    var expected = since
    for second in 1...3 {
      expected = since.addingTimeInterval(TimeInterval(second))
      wall.withLock { $0 = expected }
      manual.advance(by: .seconds(1))
      await TestSupport.waitUntil("tick \(second)") { clock.now == expected }
      XCTAssertEqual(clock.elapsed(since: since), TimeInterval(second))
      _ = await manual.waitForSleepers(1)
    }

    clock.update(for: .stopping)
    XCTAssertFalse(clock.isTicking)
    await TestSupport.waitUntil("the sleeper to be cancelled") { manual.pendingSleepers == 0 }
    wall.withLock { $0 = since.addingTimeInterval(60) }
    manual.advance(by: .seconds(5))
    await TestSupport.settle()
    XCTAssertEqual(clock.now, expected, "no tick after the recording ended")
    XCTAssertEqual(clock.elapsed(since: since.addingTimeInterval(600)), 0, "never negative")
  }
}
