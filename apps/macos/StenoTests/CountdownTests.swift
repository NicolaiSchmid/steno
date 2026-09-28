import StenoCore
import XCTest

/// The shared `Countdown` on `ManualClock`: the fraction drains one linear
/// step per tick, the text reads `m:ss`, `onElapsed` fires once and
/// `cancel()` stops the ticks. The detection prompt's own test proves the
/// prompt still closes through this type.
@MainActor
final class CountdownTests: XCTestCase {
  func testFractionDrainsOneStepPerTickAndElapsesOnce() async throws {
    let clock = ManualClock()
    var elapsed = 0
    let countdown = Countdown(duration: .seconds(3), clock: clock) { elapsed += 1 }
    XCTAssertEqual(countdown.fractionRemaining, 1)
    XCTAssertEqual(countdown.remainingText, "0:03")
    countdown.begin()
    countdown.begin()
    XCTAssertTrue(await clock.waitForSleepers(1), "one ticker, armed once")
    XCTAssertEqual(clock.pendingSleepers, 1)

    for expected in [2.0 / 3.0, 1.0 / 3.0] {
      clock.advance(by: .seconds(1))
      await TestSupport.waitUntil("tick to \(expected)") {
        abs(countdown.fractionRemaining - expected) < 0.0001
      }
      XCTAssertEqual(elapsed, 0)
      _ = await clock.waitForSleepers(1)
    }
    clock.advance(by: .seconds(1))
    await TestSupport.waitUntil("elapsed") { countdown.hasElapsed }
    XCTAssertEqual(countdown.fractionRemaining, 0)
    XCTAssertEqual(countdown.remaining, .zero)
    XCTAssertEqual(countdown.remainingText, "0:00")
    XCTAssertEqual(elapsed, 1, "fires once")
    await TestSupport.settle()
    XCTAssertEqual(clock.pendingSleepers, 0, "no ticker after elapsing")
    countdown.begin()
    await TestSupport.settle()
    XCTAssertEqual(elapsed, 1, "a second begin after elapsing does nothing")
  }

  func testRemainingTextFormatsMinutesAndSeconds() {
    let clock = ManualClock()
    XCTAssertEqual(Countdown(duration: .seconds(89), clock: clock).remainingText, "1:29")
    XCTAssertEqual(Countdown(duration: .seconds(5), clock: clock).remainingText, "0:05")
    XCTAssertEqual(Countdown(duration: .seconds(600), clock: clock).remainingText, "10:00")
    let presentation = Countdown(duration: .seconds(60), clock: clock).presentation
    XCTAssertEqual(presentation, CountdownPresentation(remainingText: "1:00", fractionRemaining: 1))
    XCTAssertEqual(CountdownPresentation.fraction(remaining: .seconds(5), duration: .zero), 0)
    XCTAssertEqual(CountdownPresentation.fraction(remaining: .seconds(9), duration: .seconds(3)), 1)
  }

  func testCancelStopsTheTicks() async throws {
    let clock = ManualClock()
    var elapsed = 0
    let countdown = Countdown(duration: .seconds(2), clock: clock) { elapsed += 1 }
    countdown.begin()
    XCTAssertTrue(await clock.waitForSleepers(1))
    countdown.cancel()
    await TestSupport.waitUntil("the sleeper to be cancelled") { clock.pendingSleepers == 0 }
    clock.advance(by: .seconds(5))
    await TestSupport.settle()
    XCTAssertEqual(countdown.remaining, .seconds(2), "no tick after cancel")
    XCTAssertFalse(countdown.hasElapsed)
    XCTAssertEqual(elapsed, 0)
  }
}
