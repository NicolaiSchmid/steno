import Foundation
import StenoCore

/// The 48 kHz synthetic signals the audio tests and `steno dev aec-bench
/// --synthetic` use: tones and sweeps from integer phase accumulators, a
/// speech-like far-end from seeded SplitMix64 noise, a seeded room impulse
/// response, and the echoed microphone built from them. Deterministic on
/// every machine; never committed (core's fixture manifest covers 16 kHz
/// files only), always built in test setup.
public enum AudioFixtures {
  public static let sampleRate: Double = 48_000

  /// A sine of `frequency` for `seconds` at `amplitude`.
  public static func tone(frequency: Double, seconds: Double, amplitude: Double = 0.5) -> [Float] {
    let count = Int(seconds * sampleRate)
    let increment = phaseIncrement(frequency)
    var phase: UInt32 = 0
    var samples = [Float](repeating: 0, count: count)
    for index in 0..<count {
      samples[index] = Float(amplitude * sine(phase))
      phase = phase &+ increment
    }
    return samples
  }

  /// A linear sine sweep.
  public static func sweep(from start: Double, to end: Double, seconds: Double, amplitude: Double)
    -> [Float]
  {
    let count = Int(seconds * sampleRate)
    var phase: UInt32 = 0
    var samples = [Float](repeating: 0, count: count)
    for index in 0..<count {
      let progress = Double(index) / Double(max(count - 1, 1))
      samples[index] = Float(amplitude * sine(phase))
      phase = phase &+ phaseIncrement(start + (end - start) * progress)
    }
    return samples
  }

  /// Seeded white noise in `-amplitude...amplitude`.
  public static func noise(seconds: Double, seed: UInt64, amplitude: Double) -> [Float] {
    var generator = SplitMix64(seed: seed)
    let count = Int(seconds * sampleRate)
    return (0..<count).map { _ in
      let unit = Double(generator.next() >> 11) / Double(1 << 53)
      return Float(amplitude * (unit * 2 - 1))
    }
  }

  /// Broadband, syllable-modulated noise: seeded noise through a one-pole
  /// low-pass (`tilt` is the pole: voice-band roll-off), gated by a 4 Hz
  /// raised-cosine envelope with a pause every fourth syllable, scaled so the
  /// loudest sample is `peak`. Enough excitation for an adaptive filter to
  /// converge on; no voice.
  public static func speechLikeFar(
    seconds: Double, seed: UInt64 = 0x5EED_0048, peak: Double = 0.7, tilt: Float = 0.7
  ) -> [Float] {
    let raw = noise(seconds: seconds, seed: seed, amplitude: 1)
    var samples = [Float](repeating: 0, count: raw.count)
    var state: Float = 0
    let syllable = sampleRate / 4
    for index in 0..<raw.count {
      state = tilt * state + (1 - tilt) * raw[index]
      let position = Double(index).truncatingRemainder(dividingBy: syllable) / syllable
      let syllableIndex = Int(Double(index) / syllable)
      let envelope = syllableIndex % 4 == 3 ? 0.0 : 0.5 * (1 - cos(2 * Double.pi * position))
      samples[index] = Float(envelope) * state
    }
    let loudest = samples.reduce(0) { max($0, abs($1)) }
    guard loudest > 0 else { return samples }
    let scale = Float(peak) / loudest
    return samples.map { $0 * scale }
  }

  /// A room: unit direct path followed by `reflections` seeded discrete
  /// reflections within `seconds`, exponentially decaying to -60 dB at the
  /// end. Sparse, so convolving six seconds with it stays cheap in a debug
  /// build.
  public static func roomImpulseResponse(
    seconds: Double = 0.1, reflections: Int = 48, reflectionGain: Double = 0.1,
    seed: UInt64 = 0x1200_0000
  ) -> [Float] {
    let count = Int(seconds * sampleRate)
    var generator = SplitMix64(seed: seed)
    var response = [Float](repeating: 0, count: count)
    response[0] = 1
    let decay = -3 * log(10.0) / Double(count)  // -60 dB at the end
    for _ in 0..<reflections {
      let position = 1 + Int(generator.next() % UInt64(count - 1))
      let unit = Double(generator.next() >> 11) / Double(1 << 53)
      response[position] += Float(reflectionGain * exp(decay * Double(position)) * (unit * 2 - 1))
    }
    return response
  }

  /// The microphone in a call with nobody talking: the far-end through the
  /// room after `delay`, plus seeded noise at `noiseAmplitude` (0.001 is
  /// -60 dBFS, a quiet room's microphone floor).
  public static func echoMic(
    far: [Float], impulseResponse: [Float], delay: TimeInterval = 0.060,
    echoGain: Double = 0.5, noiseSeed: UInt64 = 0x0A0B_0C0D, noiseAmplitude: Double = 0.001
  ) -> [Float] {
    let delayed = EchoMetrics.convolve(
      far, impulseResponse: impulseResponse, delay: Int((delay * sampleRate).rounded()))
    let floor = noise(
      seconds: Double(far.count) / sampleRate, seed: noiseSeed, amplitude: noiseAmplitude)
    return zip(delayed, floor).map { Float(echoGain) * $0 + $1 }
  }

  /// Writes 48 kHz mono Int16 WAV (for `aec-bench` and manual checks).
  public static func writeWAV(_ samples: [Float], to url: URL) throws {
    try WAVWriter.data(WAVWriter.int16(samples), sampleRate: Int(sampleRate)).write(
      to: url, options: .atomic)
  }

  /// Writes 48 kHz mono Float32 CAF in the recording writer's master
  /// format; the bake-off CLI tests decode it through
  /// `AVFoundationAudioCodec`.
  public static func writeCAF(_ samples: [Float], to url: URL) throws {
    let writer = try CAFStreamWriter(url: url, sampleRate: sampleRate, channels: 1)
    try samples.withUnsafeBufferPointer { buffer in
      if let base = buffer.baseAddress {
        try writer.write(interleaved: base, frameCount: buffer.count)
      }
    }
    try writer.finish()
  }

  static func phaseIncrement(_ frequency: Double) -> UInt32 {
    UInt32((frequency / sampleRate * 4_294_967_296.0).rounded())
  }

  static func sine(_ phase: UInt32) -> Double {
    sin(2 * Double.pi * Double(phase) / 4_294_967_296.0)
  }
}
