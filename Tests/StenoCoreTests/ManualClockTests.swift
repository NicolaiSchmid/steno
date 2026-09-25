import Foundation
import Testing

@testable import StenoCore

@Suite struct ManualClockTests {
  actor Counter {
    var value = 0
    func increment() { value += 1 }
  }

  @Test func advanceWakesASleeperExactlyOnce() async throws {
    let clock = ManualClock()
    let wakes = Counter()
    let sleeper = Task {
      try await clock.sleep(for: .seconds(2))
      await wakes.increment()
    }
    #expect(await clock.waitForSleepers(1))
    clock.advance(by: .seconds(1))
    #expect(clock.pendingSleepers == 1)
    #expect(await wakes.value == 0)
    clock.advance(by: .seconds(1))
    try await sleeper.value
    #expect(await wakes.value == 1)
    clock.advance(by: .seconds(5))
    #expect(await wakes.value == 1)
    #expect(clock.now == ManualClock.Instant(offset: .seconds(7)))
  }

  @Test func sleepersWakeInDeadlineOrder() async throws {
    let clock = ManualClock()
    let order = OrderLog()
    let late = Task {
      try await clock.sleep(for: .seconds(3))
      await order.append("late")
    }
    let early = Task {
      try await clock.sleep(for: .seconds(1))
      await order.append("early")
    }
    #expect(await clock.waitForSleepers(2))
    clock.advance(by: .seconds(1))
    try await early.value
    #expect(await order.entries == ["early"])
    clock.advance(by: .seconds(2))
    try await late.value
    #expect(await order.entries == ["early", "late"])
  }

  @Test func cancellationThrowsAndRemovesTheSleeper() async throws {
    let clock = ManualClock()
    let sleeper = Task {
      try await clock.sleep(for: .seconds(1))
    }
    #expect(await clock.waitForSleepers(1))
    sleeper.cancel()
    await #expect(throws: CancellationError.self) { try await sleeper.value }
    #expect(clock.pendingSleepers == 0)
  }

  @Test func pastDeadlinesReturnImmediately() async throws {
    let clock = ManualClock()
    try await clock.sleep(until: clock.now)
    try await clock.sleep(for: .zero)
    #expect(clock.pendingSleepers == 0)
  }

  actor OrderLog {
    var entries: [String] = []
    func append(_ entry: String) { entries.append(entry) }
  }
}
