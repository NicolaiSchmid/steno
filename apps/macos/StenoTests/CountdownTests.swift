import StenoCore
import XCTest

/// The shared countdown on `ManualClock`: the detection prompt and the
/// recorder's auto-stop both run on it.
@MainActor
final class CountdownTests: XCTestCase {
  func testFractionAndTextStepDownAndElapseOnce() async throws {
    let clock = ManualClock()
    var elapsed = 0
    let countdown = Countdown(duration: .seconds(3), clock: clock) { elapsed += 1 }
    XCTAssertEqual(countdown.fractionRemaining, 1)
    XCTAssertEqual(countdown.remainingText, "0:03")
    XCTAssertFalse(countdown.isRunning)

    countdown.begin()
    XCTAssertTrue(countdown.isRunning)
    countdown.begin()
    let sleeping = await clock.waitForSleepers(1)
    XCTAssertTrue(sleeping)
    XCTAssertEqual(clock.pendingSleepers, 1, "one tick loop, whatever `begin` is called")

    for (fraction, text) in [(2.0 / 3.0, "0:02"), (1.0 / 3.0, "0:01")] {
      await TestSupport.tick(clock, seconds: 1)
      await TestSupport.waitUntil("tick to \(text)") { countdown.remainingText == text }
      XCTAssertEqual(countdown.fractionRemaining, fraction, accuracy: 1e-9)
      XCTAssertEqual(elapsed, 0)
      XCTAssertFalse(countdown.hasElapsed)
    }
    await TestSupport.tick(clock, seconds: 1)
    await TestSupport.waitUntil("elapsed") { countdown.hasElapsed }
    XCTAssertEqual(countdown.fractionRemaining, 0)
    XCTAssertEqual(countdown.remainingText, "0:00")
    XCTAssertEqual(elapsed, 1)
    XCTAssertFalse(countdown.isRunning)
    XCTAssertEqual(clock.pendingSleepers, 0)

    countdown.begin()
    XCTAssertFalse(countdown.isRunning, "an elapsed countdown does not restart")
    clock.advance(by: .seconds(5))
    await TestSupport.settle()
    XCTAssertEqual(elapsed, 1, "fires once")
  }

  func testCancelStopsTheTicksAndNeverElapses() async throws {
    let clock = ManualClock()
    var elapsed = 0
    let countdown = Countdown(duration: .seconds(2), clock: clock) { elapsed += 1 }
    countdown.begin()
    await TestSupport.tick(clock, seconds: 1)
    await TestSupport.waitUntil("one tick") { countdown.remainingText == "0:01" }
    _ = await clock.waitForSleepers(1)

    countdown.cancel()
    XCTAssertFalse(countdown.isRunning)
    XCTAssertEqual(clock.pendingSleepers, 0, "the sleeper is withdrawn at once")
    clock.advance(by: .seconds(10))
    await TestSupport.settle()
    XCTAssertEqual(elapsed, 0)
    XCTAssertFalse(countdown.hasElapsed)
    XCTAssertEqual(countdown.remainingText, "0:01", "remaining keeps its last value")
    countdown.cancel()
    XCTAssertFalse(countdown.isRunning, "a second cancel is a no-op")
  }

  /// A ticker cancelled after its sleep resumed but before it got the main
  /// actor back must not mistake a fresh `begin()`'s ticker for itself: one
  /// loop, one decrement per second.
  func testCancelThenBeginLeavesOneTicker() async throws {
    let clock = ManualClock()
    let countdown = Countdown(duration: .seconds(3), clock: clock)
    countdown.begin()
    _ = await clock.waitForSleepers(1)
    // No suspension between these three: the stale task wakes to find itself
    // cancelled and the new ticker already installed.
    clock.advance(by: .seconds(1))
    countdown.cancel()
    countdown.begin()
    await TestSupport.waitUntil("the new ticker sleeps") { clock.pendingSleepers == 1 }
    await TestSupport.settle()
    XCTAssertEqual(countdown.remainingText, "0:03", "the stale task did not tick")
    XCTAssertEqual(clock.pendingSleepers, 1, "one loop")

    await TestSupport.tick(clock, seconds: 1)
    await TestSupport.waitUntil("one tick") { countdown.remainingText == "0:02" }
    await TestSupport.settle()
    XCTAssertEqual(countdown.remainingText, "0:02", "one decrement per second")
    XCTAssertEqual(clock.pendingSleepers, 1)
  }

  /// A countdown dropped without `cancel()` takes its sleeper with it.
  func testDroppingARunningCountdownWithdrawsItsSleeper() async throws {
    let clock = ManualClock()
    var countdown: Countdown? = Countdown(duration: .seconds(5), clock: clock)
    countdown?.begin()
    _ = await clock.waitForSleepers(1)
    XCTAssertEqual(clock.pendingSleepers, 1)
    countdown = nil
    await TestSupport.waitUntil("the sleeper is withdrawn") { clock.pendingSleepers == 0 }
  }

  func testRemainingTextFormatsMinutesAndSeconds() {
    let clock = ManualClock()
    XCTAssertEqual(Countdown(duration: .seconds(90), clock: clock).remainingText, "1:30")
    XCTAssertEqual(Countdown(duration: .seconds(89), clock: clock).remainingText, "1:29")
    XCTAssertEqual(Countdown(duration: .seconds(5), clock: clock).remainingText, "0:05")
    XCTAssertEqual(Countdown(duration: .seconds(600), clock: clock).remainingText, "10:00")
    XCTAssertEqual(Countdown(duration: .zero, clock: clock).fractionRemaining, 0)
    XCTAssertEqual(
      Countdown(duration: .seconds(90), clock: clock).presentation,
      CountdownPresentation(remainingText: "1:30", fractionRemaining: 1))
  }

  /// `onElapsed` can be pointed at its owner after that owner's `init`,
  /// which is how the prompt and the recorder wire it.
  func testOnElapsedCanBeAssignedAfterInit() async throws {
    let clock = ManualClock()
    var fired = false
    let countdown = Countdown(duration: .seconds(1), clock: clock)
    countdown.onElapsed = { fired = true }
    countdown.begin()
    await TestSupport.tick(clock, seconds: 1)
    await TestSupport.waitUntil("elapsed") { countdown.hasElapsed }
    XCTAssertTrue(fired)
  }
}
