import Foundation
import StenoCore
import Testing

@testable import StenoAudio

/// The Speex canceller on the synthetic echo fixtures: a speech-like far-end
/// through a seeded room at 60 ms plus a -50 dBFS noise floor.
@Suite struct SpeexEchoCancellerTests {
  static let seconds = 6.0
  static let frame = 480
  static let far = AudioFixtures.speechLikeFar(seconds: seconds)
  static let room = AudioFixtures.roomImpulseResponse()
  static let echoOnly = AudioFixtures.echoMic(far: far, impulseResponse: room)

  @Test func reachesTwentyDecibelsERLEAfterThreeSeconds() throws {
    let canceller = try SpeexEchoCanceller(sampleRate: 48_000, frameSize: Self.frame)
    #expect(canceller.tailLength == 9_600)
    let out = EchoMetrics.run(
      canceller, nearEnd: Self.echoOnly, farEnd: Self.far, frameSize: Self.frame)
    let erle = EchoMetrics.erle(
      nearEnd: Self.echoOnly, processed: out, range: (3 * 48_000)..<(6 * 48_000))
    #expect(erle >= 20, "ERLE \(erle) dB")
    #expect(out.count == Self.echoOnly.count)
  }

  @Test func doubleTalkKeepsTheNearEndSweep() throws {
    // Nobody talks for three seconds, then a local sweep joins the echo.
    let sweep = AudioFixtures.sweep(from: 300, to: 3_000, seconds: 3, amplitude: 0.3)
    var nearEnd = Self.echoOnly
    for (index, sample) in sweep.enumerated() {
      nearEnd[3 * 48_000 + index] += sample
    }
    let canceller = try SpeexEchoCanceller(sampleRate: 48_000, frameSize: Self.frame)
    let out = EchoMetrics.run(canceller, nearEnd: nearEnd, farEnd: Self.far, frameSize: Self.frame)
    let range = Int(3.5 * 48_000)..<(6 * 48_000)
    let sweepLevel = EchoMetrics.decibels(
      EchoMetrics.rms(sweep[(range.lowerBound - 3 * 48_000)..<(range.upperBound - 3 * 48_000)]))
    let outputLevel = EchoMetrics.decibels(EchoMetrics.rms(out[range]))
    #expect(
      abs(outputLevel - sweepLevel) <= 3, "sweep \(sweepLevel) dBFS, output \(outputLevel) dBFS")
  }

  @Test func resetForgetsTheFilterAndPassthroughDoesNothing() throws {
    let canceller = try SpeexEchoCanceller(sampleRate: 48_000, frameSize: Self.frame)
    let twoSeconds = Array(Self.echoOnly[..<(2 * 48_000)])
    let farTwo = Array(Self.far[..<(2 * 48_000)])
    let converged = EchoMetrics.run(
      canceller, nearEnd: twoSeconds, farEnd: farTwo, frameSize: Self.frame)
    canceller.reset()
    let afterReset = EchoMetrics.run(
      canceller, nearEnd: twoSeconds, farEnd: farTwo, frameSize: Self.frame)
    // The first half-second after a reset cancels less than the same half
    // second of a converged filter would.
    let range = 0..<(48_000 / 2)
    let convergedAgain = EchoMetrics.run(
      canceller, nearEnd: twoSeconds, farEnd: farTwo, frameSize: Self.frame)
    let early = EchoMetrics.erle(nearEnd: twoSeconds, processed: afterReset, range: range)
    let warm = EchoMetrics.erle(nearEnd: twoSeconds, processed: convergedAgain, range: range)
    #expect(warm > early, "warm \(warm) dB vs after reset \(early) dB")
    #expect(converged.count == twoSeconds.count)

    let passthrough = try PassthroughEchoCanceller(sampleRate: 48_000, frameSize: Self.frame)
    let unchanged = EchoMetrics.run(
      passthrough, nearEnd: twoSeconds, farEnd: farTwo, frameSize: Self.frame)
    #expect(unchanged == twoSeconds)
  }

  @Test func rejectsImpossibleShapes() {
    #expect(throws: EchoCancellerError.self) {
      try SpeexEchoCanceller(sampleRate: 48_000, frameSize: 480, tailLength: 100)
    }
    #expect(throws: EchoCancellerError.self) {
      try SpeexEchoCanceller(sampleRate: 48_000, frameSize: 0, tailLength: 100)
    }
  }

  @Test func shortBuffersArePaddedAndClamped() throws {
    let canceller = try SpeexEchoCanceller(sampleRate: 48_000, frameSize: 480)
    let loud = [Float](repeating: 2, count: 100)
    var out = [Float](repeating: 9, count: 480)
    loud.withUnsafeBufferPointer { near in
      out.withUnsafeMutableBufferPointer { destination in
        canceller.process(nearEnd: near, farEnd: near, out: destination)
      }
    }
    #expect(out.allSatisfy { $0 <= 1 && $0 >= -1 })
    #expect(out[479] != 9, "every output sample is written")
  }

  @Test func fixturesAreDeterministic() {
    #expect(AudioFixtures.speechLikeFar(seconds: 0.1) == AudioFixtures.speechLikeFar(seconds: 0.1))
    #expect(AudioFixtures.roomImpulseResponse() == Self.room)
    #expect(Self.room[0] == 1)
    #expect(Self.room.count == 4_800)
    #expect(abs(Self.room[4_799]) < 0.001)
    let tone = AudioFixtures.tone(frequency: 1_000, seconds: 1)
    #expect(tone.count == 48_000)
    #expect(abs(EchoMetrics.decibels(EchoMetrics.rms(tone)) - -9.03) < 0.05)
    let farLevel = EchoMetrics.decibels(EchoMetrics.rms(Self.far))
    #expect(farLevel > -30 && farLevel < -6, "far-end at \(farLevel) dBFS")
    let echoLevel = EchoMetrics.decibels(EchoMetrics.rms(Self.echoOnly))
    #expect(echoLevel > -40 && echoLevel < -6, "echo at \(echoLevel) dBFS")
  }
}
