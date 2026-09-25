import Foundation
import Testing

@testable import StenoAudio

@Suite struct Resampler48kTo16kTests {
  func sine(frequency: Double, amplitude: Float = 0.5, seconds: Double = 1) -> [Float] {
    let count = Int(seconds * 48_000)
    return (0..<count).map {
      amplitude * Float(sin(2 * Double.pi * frequency * Double($0) / 48_000))
    }
  }

  /// Runs `input` frame by frame through one resampler (the sidecar path) and
  /// returns the Int16 output.
  func resampleInt16(_ input: [Float], reset resetAtFrame: Int? = nil) -> [Int16] {
    let resampler = Resampler48kTo16k()
    var output: [Int16] = []
    var frame = [Int16](repeating: 0, count: 160)
    for (index, start) in stride(from: 0, to: input.count - 479, by: 480).enumerated() {
      if index == resetAtFrame { resampler.reset() }
      input.withUnsafeBufferPointer { buffer in
        frame.withUnsafeMutableBufferPointer { out in
          resampler.process(buffer.baseAddress! + start, into: out.baseAddress!)
        }
      }
      output.append(contentsOf: frame)
    }
    return output
  }

  func resample(_ input: [Float]) -> [Float] {
    resampleInt16(input).map { Float($0) / 32767 }
  }

  func rms(_ samples: ArraySlice<Float>) -> Float {
    Float((samples.reduce(0.0) { $0 + Double($1 * $1) } / Double(samples.count)).squareRoot())
  }

  @Test func oneKilohertzKeepsItsLevelAndPeriod() {
    let output = resample(sine(frequency: 1_000))
    #expect(output.count == 16_000)
    // Skip the filter delay, then compare against 0.5 / sqrt 2.
    let steady = output[2_000...]
    let expected: Float = 0.5 / Float(2.0.squareRoot())
    let error = 20 * log10(rms(steady) / expected)
    #expect(abs(error) < 0.1, "\(error) dB")
    let crossings = zip(steady, steady.dropFirst()).filter { $0 < 0 && $1 >= 0 }.count
    let seconds = Double(steady.count) / 16_000
    #expect(abs(Double(crossings) / seconds - 1_000) < 5)
    #expect(steady.max()! <= 0.5 + 0.005)
  }

  @Test func passbandEdgeSurvivesAndStopbandIsRejected() {
    let sixKilohertz = resample(sine(frequency: 6_000))
    let sixError = 20 * log10(rms(sixKilohertz[2_000...]) / (0.5 / Float(2.0.squareRoot())))
    #expect(abs(sixError) < 0.5, "6 kHz: \(sixError) dB")

    let twelveKilohertz = resample(sine(frequency: 12_000))
    let rejection = 20 * log10(rms(twelveKilohertz[2_000...]) / (0.5 / Float(2.0.squareRoot())))
    #expect(rejection < -60, "12 kHz aliases at \(rejection) dB")

    let nineKilohertz = resample(sine(frequency: 9_000))
    let nine = 20 * log10(rms(nineKilohertz[2_000...]) / (0.5 / Float(2.0.squareRoot())))
    #expect(nine < -40, "9 kHz aliases at \(nine) dB")
  }

  /// Phase across chunks: processed 480 samples at a time, the output is the
  /// input low-passed and delayed by exactly half the filter (95.5 input
  /// samples, 31.83 output samples), with no seam at any frame boundary. A
  /// history shift off by one sample would show as a 0.13 rad phase error at
  /// 1 kHz, far above the tolerance.
  @Test func outputFollowsTheInputWithAFixedDelayAcrossFrameBoundaries() {
    let output = resample(sine(frequency: 1_000))
    let delay = Double(Resampler48kTo16k.taps - 1) / 2 / Double(Resampler48kTo16k.factor)
    var maxError: Float = 0
    for index in 500..<16_000 {
      let ideal = 0.5 * Float(sin(2 * Double.pi * 1_000 * (Double(index) - delay) / 16_000))
      maxError = max(maxError, abs(output[index] - ideal))
    }
    #expect(maxError < 0.01, "largest deviation from the delayed sine: \(maxError)")
  }

  @Test func outputClampsToFullScale() {
    let ints = resampleInt16(sine(frequency: 440, amplitude: 1.2, seconds: 0.1))
    #expect(ints.count == 1_600)
    #expect(ints.max()! == 32767)
    #expect(ints.min()! == -32767)
  }

  @Test func historyCarriesAcrossFramesAndResets() {
    let input = sine(frequency: 1_000, seconds: 0.05)
    let whole = resampleInt16(input)
    // The same signal with a reset before the third frame: identical until
    // the reset, different after it.
    let interrupted = resampleInt16(input, reset: 2)
    #expect(Array(interrupted[..<320]) == Array(whole[..<320]))
    #expect(Array(interrupted[320..<480]) != Array(whole[320..<480]))
  }

  @Test func filterHasUnityDCGainAndSymmetry() {
    let taps = Resampler48kTo16k.kaiserSinc(taps: 192, cutoff: 7_300 / 48_000, beta: 9)
    #expect(abs(taps.reduce(0, +) - 1) < 1e-5)
    for index in 0..<96 {
      #expect(abs(taps[index] - taps[191 - index]) < 1e-7)
    }
  }
}
