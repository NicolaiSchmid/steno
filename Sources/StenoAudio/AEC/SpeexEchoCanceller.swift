import Foundation
import StenoCore
import speex

public enum EchoCancellerError: Error, Sendable, Equatable, CustomStringConvertible {
  case initialisationFailed(String)

  public var description: String {
    switch self {
    case .initialisationFailed(let detail): "echo canceller could not start: \(detail)"
    }
  }
}

/// SpeexDSP's MDF adaptive filter (`speex_echo_cancellation`) followed by its
/// preprocessor for residual echo suppression, at 48 kHz on 10 ms frames with
/// a 200 ms tail by default. Float in and out; Int16 scratch buffers for the
/// C API are allocated once in `init`. `process` allocates nothing and takes
/// no locks: Speex works inside the state it allocated up front. One frame
/// of far-end per frame of near-end, captured at the same instant; the
/// processing thread owns any extra delay.
public final class SpeexEchoCanceller: EchoCanceller, @unchecked Sendable {
  public let sampleRate: Double
  public let frameSize: Int
  public let tailLength: Int
  private let echo: OpaquePointer
  private let preprocess: OpaquePointer
  private let nearScratch: UnsafeMutablePointer<Int16>
  private let farScratch: UnsafeMutablePointer<Int16>
  private let outScratch: UnsafeMutablePointer<Int16>

  /// Tail 200 ms of `sampleRate`.
  public convenience init(sampleRate: Double, frameSize: Int) throws {
    try self.init(sampleRate: sampleRate, frameSize: frameSize, tailLength: Int(sampleRate * 0.2))
  }

  /// `residualSuppression` is the preprocessor's echo suppression in dB
  /// (negative), `residualSuppressionActive` the same while near-end speech
  /// is present; Speex's defaults are -40 and -15.
  public init(
    sampleRate: Double, frameSize: Int, tailLength: Int, residualSuppression: Int32 = -40,
    residualSuppressionActive: Int32 = -15
  ) throws {
    guard frameSize > 0, tailLength >= frameSize else {
      throw EchoCancellerError.initialisationFailed(
        "frame \(frameSize) and tail \(tailLength) must be positive with tail >= frame")
    }
    guard let echo = speex_echo_state_init(Int32(frameSize), Int32(tailLength)) else {
      throw EchoCancellerError.initialisationFailed("speex_echo_state_init returned nil")
    }
    var rate = Int32(sampleRate)
    speex_echo_ctl(echo, SPEEX_ECHO_SET_SAMPLING_RATE, &rate)
    guard let preprocess = speex_preprocess_state_init(Int32(frameSize), Int32(sampleRate)) else {
      speex_echo_state_destroy(echo)
      throw EchoCancellerError.initialisationFailed("speex_preprocess_state_init returned nil")
    }
    speex_preprocess_ctl(preprocess, SPEEX_PREPROCESS_SET_ECHO_STATE, UnsafeMutableRawPointer(echo))
    var denoise: Int32 = 0
    speex_preprocess_ctl(preprocess, SPEEX_PREPROCESS_SET_DENOISE, &denoise)
    var agc: Int32 = 0
    speex_preprocess_ctl(preprocess, SPEEX_PREPROCESS_SET_AGC, &agc)
    var vad: Int32 = 0
    speex_preprocess_ctl(preprocess, SPEEX_PREPROCESS_SET_VAD, &vad)
    var suppress = residualSuppression
    speex_preprocess_ctl(preprocess, SPEEX_PREPROCESS_SET_ECHO_SUPPRESS, &suppress)
    var suppressActive = residualSuppressionActive
    speex_preprocess_ctl(preprocess, SPEEX_PREPROCESS_SET_ECHO_SUPPRESS_ACTIVE, &suppressActive)

    self.sampleRate = sampleRate
    self.frameSize = frameSize
    self.tailLength = tailLength
    self.echo = echo
    self.preprocess = preprocess
    nearScratch = .allocate(capacity: frameSize)
    nearScratch.initialize(repeating: 0, count: frameSize)
    farScratch = .allocate(capacity: frameSize)
    farScratch.initialize(repeating: 0, count: frameSize)
    outScratch = .allocate(capacity: frameSize)
    outScratch.initialize(repeating: 0, count: frameSize)
  }

  deinit {
    speex_preprocess_state_destroy(preprocess)
    speex_echo_state_destroy(echo)
    nearScratch.deallocate()
    farScratch.deallocate()
    outScratch.deallocate()
  }

  /// `nearEnd`, `farEnd` and `out` must hold `frameSize` samples; shorter
  /// buffers are zero-padded, longer ones truncated.
  public func process(
    nearEnd: UnsafeBufferPointer<Float>,
    farEnd: UnsafeBufferPointer<Float>,
    out: UnsafeMutableBufferPointer<Float>
  ) {
    Self.toInt16(nearEnd, into: nearScratch, count: frameSize)
    Self.toInt16(farEnd, into: farScratch, count: frameSize)
    speex_echo_cancellation(echo, nearScratch, farScratch, outScratch)
    speex_preprocess_run(preprocess, outScratch)
    let count = min(frameSize, out.count)
    guard let destination = out.baseAddress else { return }
    var index = 0
    while index < count {
      destination[index] = Float(outScratch[index]) / 32768
      index += 1
    }
  }

  /// Forgets the adaptive filter (a device change, a new recording).
  public func reset() {
    speex_echo_state_reset(echo)
  }

  @inline(__always)
  private static func toInt16(
    _ source: UnsafeBufferPointer<Float>, into destination: UnsafeMutablePointer<Int16>, count: Int
  ) {
    let available = min(count, source.count)
    var index = 0
    if let base = source.baseAddress {
      while index < available {
        let scaled = base[index] * 32767
        destination[index] = Int16(max(-32768, min(32767, scaled.rounded())))
        index += 1
      }
    }
    while index < count {
      destination[index] = 0
      index += 1
    }
  }
}
