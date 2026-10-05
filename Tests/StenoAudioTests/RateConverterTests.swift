import Foundation
import Testing

@testable import StenoAudio

/// The streaming converter from a device's rate to 48 kHz
/// (`.plans/2026-10-05-device-sample-rate.md`): chunking changes nothing, a
/// tone keeps its level and frequency from every rate a Mac device runs at,
/// and the timing holds. Rust: `crates/steno-audio/tests/rate_converter.rs`.
@Suite struct RateConverterTests {
  /// `seconds` of a sine of `hertz` at amplitude 0.5, sampled at `rate`.
  func tone(_ hertz: Double, at rate: Double, seconds: Double = 1) -> [Float] {
    (0..<Int(rate * seconds)).map {
      0.5 * Float(sin(2 * Double.pi * hertz * Double($0) / rate))
    }
  }

  /// `input` through one converter in calls of `chunks` samples in turn.
  func convert(_ input: [Float], from rate: Double, chunks: [Int]) -> [Float] {
    let converter = RateConverter(
      inputRate: rate, outputRate: StenoAudio.sampleRate, maximumInput: chunks.max()!)
    var scratch = [Float](repeating: 0, count: converter.maximumOutput)
    var output: [Float] = []
    var offset = 0
    var chunk = 0
    while offset < input.count {
      let end = min(offset + chunks[chunk % chunks.count], input.count)
      let written = input.withUnsafeBufferPointer { buffer in
        scratch.withUnsafeMutableBufferPointer {
          converter.process(
            buffer.baseAddress! + offset, count: end - offset, into: $0.baseAddress!)
        }
      }
      output.append(contentsOf: scratch[..<written])
      offset = end
      chunk += 1
    }
    return output
  }

  func levelAgainstHalfScaleSine(_ samples: ArraySlice<Float>) -> Double {
    let power = samples.reduce(0.0) { $0 + Double($1) * Double($1) } / Double(samples.count)
    return 20 * log10(power.squareRoot() / (0.5 / 2.0.squareRoot()))
  }

  func frequency(_ samples: ArraySlice<Float>) -> Double {
    let crossings = zip(samples, samples.dropFirst()).filter { $0 < 0 && $1 >= 0 }.count
    return Double(crossings) / (Double(samples.count) / StenoAudio.sampleRate)
  }

  @Test(arguments: [16_000.0, 24_000.0, 44_100.0, 96_000.0])
  func chunkedInputConvertsExactlyAsOnePass(rate: Double) {
    let input = tone(1_000, at: rate)
    let whole = convert(input, from: rate, chunks: [input.count])
    let chunked = convert(input, from: rate, chunks: [1, 7, 240, 100, 33, 512])
    #expect(whole == chunked)
  }

  @Test(arguments: [8_000.0, 16_000.0, 24_000.0, 44_100.0, 96_000.0, 192_000.0])
  func aToneKeepsItsLevelFrequencyAndLength(rate: Double) {
    let output = convert(tone(1_000, at: rate), from: rate, chunks: [480])
    // The last `taps / 2` inputs wait for a next call that never comes.
    let expected = StenoAudio.sampleRate - Double(RateConverter.taps / 2) * 48_000 / rate
    #expect(abs(Double(output.count) - expected) <= 2, "\(output.count) samples")
    let steady = output[1_000..<40_000]
    #expect(abs(levelAgainstHalfScaleSine(steady)) < 0.1)
    #expect(abs(frequency(steady) - 1_000) < 2)
  }

  @Test func contentAbove48kNyquistIsRejected() {
    let output = convert(tone(30_000, at: 96_000), from: 96_000, chunks: [480])
    #expect(levelAgainstHalfScaleSine(output[1_000..<40_000]) < -50)
  }

  /// An impulse at device sample 100 lands on output sample 200 at 24 kHz
  /// and 300 at 16 kHz: the lanes keep their timing.
  @Test(arguments: [(24_000.0, 200), (16_000.0, 300)])
  func inputZeroIsOutputZero(rate: Double, at expected: Int) {
    var input = [Float](repeating: 0, count: 1_000)
    input[100] = 1
    let output = convert(input, from: rate, chunks: [160])
    let peak = output.indices.max { output[$0] < output[$1] }
    #expect(peak == expected)
  }

  @Test func resetStartsANewSignal() {
    let input = tone(1_000, at: 24_000, seconds: 0.1)
    let converter = RateConverter(
      inputRate: 24_000, outputRate: StenoAudio.sampleRate, maximumInput: input.count)
    var first = [Float](repeating: 0, count: converter.maximumOutput)
    var second = first
    let written = input.withUnsafeBufferPointer { buffer in
      first.withUnsafeMutableBufferPointer {
        converter.process(buffer.baseAddress!, count: input.count, into: $0.baseAddress!)
      }
    }
    converter.reset()
    let again = input.withUnsafeBufferPointer { buffer in
      second.withUnsafeMutableBufferPointer {
        converter.process(buffer.baseAddress!, count: input.count, into: $0.baseAddress!)
      }
    }
    #expect(again == written)
    #expect(first == second)
  }

  @Test func onlyRatesFrom8To192KilohertzAreSupported() {
    #expect(RateConverter.supports(8_000))
    #expect(RateConverter.supports(24_000))
    #expect(RateConverter.supports(192_000))
    #expect(!RateConverter.supports(0))
    #expect(!RateConverter.supports(4_000))
    #expect(!RateConverter.supports(384_000))
    #expect(!RateConverter.supports(.nan))
  }
}
