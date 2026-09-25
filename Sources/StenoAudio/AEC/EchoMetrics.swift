import Foundation
import StenoCore

/// RMS, ERLE and convolution helpers shared by the echo canceller tests,
/// `LiveAECPathTests` and `steno dev aec-bench`.
public enum EchoMetrics {
  /// Linear RMS of `samples`; 0 for an empty slice.
  public static func rms<C: Collection>(_ samples: C) -> Float where C.Element == Float {
    guard !samples.isEmpty else { return 0 }
    let sum = samples.reduce(0.0) { $0 + Double($1 * $1) }
    return Float((sum / Double(samples.count)).squareRoot())
  }

  /// dBFS of a linear magnitude; `-160` for silence.
  public static func decibels(_ linear: Float) -> Float {
    guard linear > 1e-8 else { return -160 }
    return 20 * log10(linear)
  }

  /// Echo return loss enhancement over `range`: how much quieter the
  /// processed signal is than the unprocessed near-end, in dB. Positive is
  /// better; with nothing but echo on the near-end it measures cancellation.
  public static func erle(nearEnd: [Float], processed: [Float], range: Range<Int>) -> Float {
    let bounded = range.clamped(to: 0..<min(nearEnd.count, processed.count))
    let before = rms(nearEnd[bounded])
    let after = rms(processed[bounded])
    guard before > 0 else { return 0 }
    return decibels(before) - decibels(after)
  }

  /// The linear magnitude of one frequency in `samples` (Goertzel), scaled so
  /// a full-scale sine at `frequency` reads 1. Tells how much of an
  /// independent tone survived cancellation while an echo at another
  /// frequency was removed.
  public static func toneLevel(_ samples: ArraySlice<Float>, frequency: Double, sampleRate: Double)
    -> Float
  {
    let count = samples.count
    guard count > 0 else { return 0 }
    let omega = 2 * Double.pi * frequency / sampleRate
    let coefficient = 2 * cos(omega)
    var previous = 0.0
    var beforePrevious = 0.0
    for sample in samples {
      let current = Double(sample) + coefficient * previous - beforePrevious
      beforePrevious = previous
      previous = current
    }
    let real = previous - beforePrevious * cos(omega)
    let imaginary = beforePrevious * sin(omega)
    return Float((real * real + imaginary * imaginary).squareRoot() * 2 / Double(count))
  }

  /// Direct-form convolution of `signal` with `impulseResponse`, truncated to
  /// `signal.count`, optionally delayed by `delay` samples (zeros first).
  /// Zero taps are skipped, so a sparse room costs one multiply per
  /// reflection per sample.
  public static func convolve(_ signal: [Float], impulseResponse: [Float], delay: Int = 0)
    -> [Float]
  {
    var output = [Float](repeating: 0, count: signal.count)
    let taps = impulseResponse.enumerated().filter { $0.element != 0 }
    for (tapIndex, tap) in taps {
      let shift = tapIndex + delay
      guard shift < signal.count else { continue }
      for n in shift..<signal.count {
        output[n] += signal[n - shift] * tap
      }
    }
    return output
  }

  /// Runs `canceller` over whole frames of `nearEnd` with `farEnd` and returns
  /// the processed signal (the last partial frame is dropped).
  public static func run(
    _ canceller: some EchoCanceller, nearEnd: [Float], farEnd: [Float], frameSize: Int
  ) -> [Float] {
    let frames = min(nearEnd.count, farEnd.count) / frameSize
    var output = [Float](repeating: 0, count: frames * frameSize)
    nearEnd.withUnsafeBufferPointer { near in
      farEnd.withUnsafeBufferPointer { far in
        output.withUnsafeMutableBufferPointer { out in
          for frame in 0..<frames {
            let offset = frame * frameSize
            canceller.process(
              nearEnd: UnsafeBufferPointer(rebasing: near[offset..<(offset + frameSize)]),
              farEnd: UnsafeBufferPointer(rebasing: far[offset..<(offset + frameSize)]),
              out: UnsafeMutableBufferPointer(rebasing: out[offset..<(offset + frameSize)]))
          }
        }
      }
    }
    return output
  }
}
