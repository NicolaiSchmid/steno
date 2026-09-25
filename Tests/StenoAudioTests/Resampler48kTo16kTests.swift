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

  func resample(_ input: [Float]) -> [Float] {
    let resampler = Resampler48kTo16k()
    var output: [Float] = []
    var frame = [Float](repeating: 0, count: 160)
    for start in stride(from: 0, to: input.count - 479, by: 480) {
      input.withUnsafeBufferPointer { buffer in
        frame.withUnsafeMutableBufferPointer { out in
          resampler.process(buffer.baseAddress! + start, into: out.baseAddress!)
        }
      }
      output.append(contentsOf: frame)
    }
    return output
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

  @Test func int16OutputMatchesTheFloatPathAndClamps() {
    let input = sine(frequency: 440, amplitude: 1.2, seconds: 0.1)
    let floats = resample(input)
    let resampler = Resampler48kTo16k()
    var ints: [Int16] = []
    var frame = [Int16](repeating: 0, count: 160)
    for start in stride(from: 0, to: input.count - 479, by: 480) {
      input.withUnsafeBufferPointer { buffer in
        frame.withUnsafeMutableBufferPointer { out in
          resampler.process(buffer.baseAddress! + start, into: out.baseAddress!)
        }
      }
      ints.append(contentsOf: frame)
    }
    #expect(ints.count == floats.count)
    for (int, float) in zip(ints, floats) {
      #expect(abs(Float(int) / 32767 - float) < 1.0 / 32767)
    }
    #expect(floats.max()! <= 1)
    #expect(ints.max()! == 32767)
  }

  @Test func historyCarriesAcrossFramesAndResets() {
    let input = sine(frequency: 1_000, seconds: 0.05)
    let whole = resample(input)
    // The same signal in one frame at a time must equal the frame-by-frame
    // output above (which already is frame-by-frame); a reset in between
    // must not.
    let resampler = Resampler48kTo16k()
    var output: [Float] = []
    var frame = [Float](repeating: 0, count: 160)
    for (index, start) in stride(from: 0, to: input.count - 479, by: 480).enumerated() {
      if index == 2 { resampler.reset() }
      input.withUnsafeBufferPointer { buffer in
        frame.withUnsafeMutableBufferPointer { out in
          resampler.process(buffer.baseAddress! + start, into: out.baseAddress!)
        }
      }
      output.append(contentsOf: frame)
    }
    #expect(Array(output[..<320]) == Array(whole[..<320]))
    #expect(Array(output[320..<480]) != Array(whole[320..<480]))
  }

  @Test func filterHasUnityDCGainAndSymmetry() {
    let taps = Resampler48kTo16k.kaiserSinc(taps: 192, cutoff: 7_300 / 48_000, beta: 9)
    #expect(abs(taps.reduce(0, +) - 1) < 1e-5)
    for index in 0..<96 {
      #expect(abs(taps[index] - taps[191 - index]) < 1e-7)
    }
  }
}
