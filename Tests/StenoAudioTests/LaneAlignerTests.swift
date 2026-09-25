import Testing

@testable import StenoAudio

@Suite struct LaneAlignerTests {
  let nanoseconds = LaneAligner(sampleRate: 48_000, nanosecondsPerHostTick: 1)
  /// Apple Silicon: 24 MHz host clock, 125/3 ns per tick.
  let appleSilicon = LaneAligner(sampleRate: 48_000, nanosecondsPerHostTick: 125.0 / 3.0)

  @Test func fiveMillisecondsIs240Frames() {
    let start: UInt64 = 1_000_000_000
    #expect(nanoseconds.frames(from: start, to: start + 5_000_000) == 240)
    #expect(nanoseconds.frames(from: start + 5_000_000, to: start) == -240)
    #expect(nanoseconds.frames(from: start, to: start) == 0)
  }

  @Test func hostTicksAreScaledByTheTimebase() {
    // 24 MHz: 120 000 ticks are 5 ms.
    let start: UInt64 = 7_000_000
    #expect(appleSilicon.frames(from: start, to: start + 120_000) == 240)
    #expect(appleSilicon.frames(from: start, to: start + 24_000_000) == 48_000)
  }

  @Test func theEarlierLaneSkipsTheDifference() {
    let micStart: UInt64 = 2_000_000_000
    let tapStart = micStart + 5_000_000
    #expect(nanoseconds.initialSkips(hostTimeA: micStart, hostTimeB: tapStart) == (240, 0))
    #expect(nanoseconds.initialSkips(hostTimeA: tapStart, hostTimeB: micStart) == (0, 240))
    #expect(nanoseconds.initialSkips(hostTimeA: micStart, hostTimeB: micStart) == (0, 0))
  }

  @Test func driftIsTheSurplusOverTheClock() {
    let start: UInt64 = 0
    let oneSecond: UInt64 = 1_000_000_000
    #expect(nanoseconds.drift(framesDelivered: 48_000, from: start, to: oneSecond) == 0)
    #expect(nanoseconds.drift(framesDelivered: 48_048, from: start, to: oneSecond) == 48)
    #expect(nanoseconds.drift(framesDelivered: 47_952, from: start, to: oneSecond) == -48)
  }

  @Test func budgetIsTwentyMilliseconds() {
    #expect(nanoseconds.isWithinBudget(offsetFrames: 960))
    #expect(nanoseconds.isWithinBudget(offsetFrames: -960))
    #expect(!nanoseconds.isWithinBudget(offsetFrames: 961))
    #expect(nanoseconds.isWithinBudget(offsetFrames: 2_400, budgetSeconds: 0.05))
  }
}
