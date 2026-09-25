import Foundation

/// 48 kHz → 16 kHz by exact 3:1 decimation through a Kaiser-windowed sinc
/// low-pass (192 taps, cutoff 7.3 kHz, stopband from about 8 kHz). Pure
/// Swift, deterministic on every machine, every buffer allocated in `init`,
/// so it runs on the writer thread without allocation and its output is
/// byte-identical between CI and a Mac. One instance per lane; it keeps the
/// filter history between calls.
final class Resampler48kTo16k: @unchecked Sendable {
  static let factor = 3
  static let taps = 192

  let frameSize: Int
  /// Output samples per call: `frameSize / 3`.
  let outputFrameSize: Int
  private let coefficients: [Float]
  private let history: UnsafeMutablePointer<Float>
  private let historyLength: Int

  /// `frameSize` must be a multiple of 3.
  init(frameSize: Int = StenoAudio.frameSize) {
    precondition(frameSize % Self.factor == 0, "frameSize must be a multiple of 3")
    self.frameSize = frameSize
    self.outputFrameSize = frameSize / Self.factor
    self.coefficients = Self.kaiserSinc(taps: Self.taps, cutoff: 7_300 / 48_000, beta: 9)
    self.historyLength = Self.taps - 1 + frameSize
    self.history = .allocate(capacity: historyLength)
    self.history.initialize(repeating: 0, count: historyLength)
  }

  deinit {
    history.deallocate()
  }

  /// Consumes exactly `frameSize` input samples and produces
  /// `outputFrameSize` Int16 samples (the sidecar format) in `output`,
  /// clamped to full scale.
  func process(_ input: UnsafePointer<Float>, into output: UnsafeMutablePointer<Int16>) {
    let taps = Self.taps
    let offset = taps - 1
    (history + offset).update(from: input, count: frameSize)
    var n = 0
    while n < outputFrameSize {
      // The newest sample of this output's window sits at 3n + offset.
      let newest = n * Self.factor + offset
      var accumulator: Float = 0
      var k = 0
      while k < taps {
        accumulator += coefficients[k] * history[newest - k]
        k += 1
      }
      output[n] = Int16(clamping: Int((min(1, max(-1, accumulator)) * 32767).rounded()))
      n += 1
    }
    // Keep the last taps - 1 input samples for the next call.
    history.update(from: history + frameSize, count: offset)
  }

  /// Zeroes the history (a new recording).
  func reset() {
    history.update(repeating: 0, count: historyLength)
  }

  /// Windowed-sinc low-pass, unity DC gain. `cutoff` is normalised to the
  /// input rate.
  static func kaiserSinc(taps: Int, cutoff: Double, beta: Double) -> [Float] {
    let centre = Double(taps - 1) / 2
    var coefficients = [Double](repeating: 0, count: taps)
    let denominator = besselI0(beta)
    for index in 0..<taps {
      let x = Double(index) - centre
      let sinc = x == 0 ? 2 * cutoff : sin(2 * Double.pi * cutoff * x) / (Double.pi * x)
      let ratio = 2 * Double(index) / Double(taps - 1) - 1
      let window = besselI0(beta * (1 - ratio * ratio).squareRoot()) / denominator
      coefficients[index] = sinc * window
    }
    let sum = coefficients.reduce(0, +)
    return coefficients.map { Float($0 / sum) }
  }

  /// Zeroth-order modified Bessel function of the first kind (series).
  static func besselI0(_ x: Double) -> Double {
    var sum = 1.0
    var term = 1.0
    let half = x / 2
    var k = 1.0
    while term > 1e-12 * sum {
      term *= (half / k) * (half / k)
      sum += term
      k += 1
    }
    return sum
  }
}
