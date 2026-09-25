import Foundation
import StenoCore
import Testing

@testable import StenoAudio

/// The pure halves of two device-setup paths that otherwise need a Mac: the
/// nominal-sample-rate read-back after the aggregate is asked for 48 kHz,
/// and the permission probe's wait for the first non-silent tap buffer.
@Suite struct DeviceSetupTests {
  @Test func aRateThatAlreadyMatchesIsConfirmedWithoutWaiting() {
    var reads = 0
    var waits = 0
    let rate = NominalSampleRate.settle(
      to: 48_000,
      read: {
        reads += 1
        return 48_000
      }, wait: { waits += 1 })
    #expect(rate == 48_000)
    #expect(reads == 1)
    #expect(waits == 0)
  }

  /// The HAL applies the new rate asynchronously: the third read sees it.
  @Test func anAsynchronousRateChangeIsWaitedFor() {
    var readings: [Double] = [44_100, 44_100, 48_000, 48_000]
    var waits = 0
    let rate = NominalSampleRate.settle(
      to: 48_000, read: { readings.removeFirst() }, wait: { waits += 1 })
    #expect(rate == 48_000)
    #expect(waits == 2)
    #expect(readings == [48_000], "stops reading once it matches")
  }

  /// A device fixed at 44.1 kHz never matches; the last rate read comes back
  /// after exactly `attempts` reads so the backend can fail with it.
  @Test func aDeviceStuckAtAnotherRateReportsItAfterTheAttempts() {
    var reads = 0
    var waits = 0
    let rate = NominalSampleRate.settle(
      to: 48_000, attempts: 4,
      read: {
        reads += 1
        return 44_100
      }, wait: { waits += 1 })
    #expect(rate == 44_100)
    #expect(reads == 4)
    #expect(waits == 3)
    #expect(NominalSampleRate.attempts * Int(NominalSampleRate.interval * 1_000) == 200, "200 ms")
  }

  /// The probe returns as soon as the tap delivers signal, however long the
  /// user took over the prompt, and keeps the tone playing meanwhile.
  @Test func theProbeSucceedsWhenSignalArrivesLate() async {
    let clock = ManualClock()
    let polls = Counter()
    let plays = Counter()
    let task = Task {
      await SystemAudioPermission.waitForSignal(
        timeout: .seconds(30), pollInterval: .seconds(1), clock: clock,
        sample: {
          polls.value += 1
          // Silence for five polls (the prompt is up), then the tone.
          return polls.value <= 5 ? [Float](repeating: 0, count: 480) : [0, 0.3, -0.3]
        },
        keepPlaying: { plays.value += 1 })
    }
    for _ in 0..<5 {
      #expect(await clock.waitForSleepers(1))
      clock.advance(by: .seconds(1))
    }
    #expect(await task.value)
    #expect(polls.value == 6)
    #expect(plays.value == 6, "the tone is (re)started before every poll")
    #expect(clock.now == ManualClock.Instant(offset: .seconds(5)))
  }

  /// Silence until the deadline means denied: the old fixed 500 ms window
  /// reported "denied" to a user who was still reading the prompt.
  @Test func theProbeFailsOnlyAtTheDeadline() async {
    let clock = ManualClock()
    let polls = Counter()
    let task = Task {
      await SystemAudioPermission.waitForSignal(
        timeout: .seconds(3), pollInterval: .seconds(1), clock: clock,
        sample: {
          polls.value += 1
          return [Float](repeating: LaneLevel.silentPeakLinear / 2, count: 480)
        },
        keepPlaying: {})
    }
    for _ in 0..<3 {
      #expect(await clock.waitForSleepers(1))
      clock.advance(by: .seconds(1))
    }
    #expect(await task.value == false)
    #expect(polls.value == 4, "polled at 0, 1, 2 and 3 s")
    #expect(clock.now == ManualClock.Instant(offset: .seconds(3)))
  }

  /// A single sample just above -80 dBFS is signal; at or below it is not.
  @Test func theSilenceThresholdIsMinus80dBFS() async {
    let clock = ManualClock()
    let loud = await SystemAudioPermission.waitForSignal(
      timeout: .zero, clock: clock, sample: { [LaneLevel.silentPeakLinear * 1.01] },
      keepPlaying: {})
    let quiet = await SystemAudioPermission.waitForSignal(
      timeout: .zero, clock: clock, sample: { [LaneLevel.silentPeakLinear] }, keepPlaying: {})
    #expect(loud)
    #expect(!quiet)
    #expect(abs(LevelMeter.decibels(LaneLevel.silentPeakLinear) - -80) < 0.01)
  }

  final class Counter: @unchecked Sendable {
    var value = 0
  }
}
