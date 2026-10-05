import Foundation

/// One lane from the device's rate to 48 kHz, converted as it streams on the
/// processing thread. The aggregate runs at whatever its clock master
/// accepts, and a Bluetooth headset in the hands-free profile accepts only
/// 24 or 16 kHz (`.plans/2026-10-05-device-sample-rate.md`).
///
/// A polyphase Kaiser-windowed sinc: 128 phases of 64 taps, cutoff 0.45 of
/// the lower rate, beta 9, adjacent phases interpolated linearly; the table
/// of `crates/steno-audio/src/codec/sinc.rs`. Output `n` sits at input
/// position `n * inputRate / outputRate`, kept as a whole index and a
/// remainder in units of `1 / outputRate`, so the position is exact over any
/// length and never drifts. The window is centred on that position and the
/// history starts with `taps / 2 - 1` zeros, so output 0 is input 0 and the
/// lane keeps its timing; the last `taps / 2` input samples wait for the
/// next call. Converters with the same rates fed the same input counts write
/// the same output counts, which keeps the lanes in step.
///
/// `init` allocates everything; `process` allocates nothing.
final class RateConverter: @unchecked Sendable {
  static let phases = 128
  static let taps = 64
  /// The lowest device rate the converter accepts: narrowband hands-free.
  static let minimumRate: Double = 8_000
  /// The highest: four input samples per output at 48 kHz, well inside one
  /// window.
  static let maximumRate: Double = 192_000

  /// Whether a device at `rate` hertz can be converted.
  static func supports(_ rate: Double) -> Bool {
    (minimumRate...maximumRate).contains(rate.rounded())
  }

  /// The most input samples one `process` call takes.
  let maximumInput: Int
  private let inputRate: Int
  private let outputRate: Int
  private let table: UnsafeMutablePointer<Float>
  /// Input not yet consumed by a window, from `history[0]`.
  private let history: UnsafeMutablePointer<Float>
  private let historyCapacity: Int
  private var filled = 0
  /// The next output's whole input position in `history`.
  private var index = 0
  /// Its fraction, in units of `1 / outputRate`.
  private var remainder = 0

  /// `inputRate` to `outputRate` hertz (rounded to whole hertz, both
  /// supported), for calls of at most `maximumInput` samples.
  init(inputRate: Double, outputRate: Double, maximumInput: Int) {
    precondition(Self.supports(inputRate) && Self.supports(outputRate))
    self.inputRate = Int(inputRate.rounded())
    self.outputRate = Int(outputRate.rounded())
    self.maximumInput = maximumInput
    let coefficients = Self.table(inputRate: inputRate.rounded(), outputRate: outputRate.rounded())
    let table = UnsafeMutablePointer<Float>.allocate(capacity: coefficients.count)
    coefficients.withUnsafeBufferPointer {
      table.initialize(from: $0.baseAddress!, count: coefficients.count)
    }
    self.table = table
    // A window never holds more than `taps - 1` samples it has not
    // consumed, so that plus one call's input always fits.
    historyCapacity = Self.taps - 1 + maximumInput
    history = .allocate(capacity: historyCapacity)
    history.initialize(repeating: 0, count: historyCapacity)
    reset()
  }

  deinit {
    table.deallocate()
    history.deallocate()
  }

  /// The most output samples one `process` call writes.
  var maximumOutput: Int {
    (maximumInput * outputRate + inputRate - 1) / inputRate + 1
  }

  /// Back to the start of a signal: zeros before input 0, position 0.
  func reset() {
    history.update(repeating: 0, count: historyCapacity)
    filled = Self.taps / 2 - 1
    index = 0
    remainder = 0
  }

  /// Converts `count` samples of `input` (at most `maximumInput`) into
  /// `output` (room for at least `maximumOutput`) and returns how many
  /// samples it wrote.
  @inline(__always)
  func process(
    _ input: UnsafePointer<Float>, count: Int, into output: UnsafeMutablePointer<Float>
  ) -> Int {
    let taps = Self.taps
    (history + filled).update(from: input, count: count)
    filled += count
    var written = 0
    while index + taps <= filled {
      let scaled = remainder * Self.phases
      let phase = scaled / outputRate
      let blend = Float(scaled % outputRate) / Float(outputRate)
      let low = table + phase * taps
      let high = low + taps
      let window = history + index
      var accumulator: Float = 0
      var k = 0
      while k < taps {
        accumulator += (low[k] + (high[k] - low[k]) * blend) * window[k]
        k += 1
      }
      output[written] = accumulator
      written += 1
      remainder += inputRate
      index += remainder / outputRate
      remainder %= outputRate
    }
    // Keep what the next window starts on.
    let consumed = min(index, filled)
    history.update(from: history + consumed, count: filled - consumed)
    filled -= consumed
    index -= consumed
    return written
  }

  /// The `phases + 1` sub-filters of `taps` coefficients for one pair of
  /// rates, each normalised to unit gain; the last equals the first shifted
  /// by a sample so interpolation never reads past the table.
  static func table(inputRate: Double, outputRate: Double) -> [Float] {
    // The low-pass sits below the lower Nyquist, normalised to the input
    // rate; when upsampling the input's own band is the limit.
    let cutoff = 0.45 * min(outputRate, inputRate) / inputRate
    let centre = Double(taps / 2)
    let denominator = Resampler48kTo16k.besselI0(9)
    var table: [Float] = []
    table.reserveCapacity((phases + 1) * taps)
    for phase in 0...phases {
      let fraction = Double(phase) / Double(phases)
      var coefficients = [Double](repeating: 0, count: taps)
      var sum = 0.0
      for k in 0..<taps {
        let x = Double(k) - centre + 1 - fraction
        let sinc = x == 0 ? 2 * cutoff : sin(2 * Double.pi * cutoff * x) / (Double.pi * x)
        let ratio = x / centre
        let window =
          abs(ratio) >= 1
          ? 0 : Resampler48kTo16k.besselI0(9 * (1 - ratio * ratio).squareRoot()) / denominator
        coefficients[k] = sinc * window
        sum += sinc * window
      }
      table.append(contentsOf: coefficients.map { Float($0 / sum) })
    }
    return table
  }
}
