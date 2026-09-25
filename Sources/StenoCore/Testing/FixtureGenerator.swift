import Foundation

/// The one synthetic-audio generator: seeded SplitMix64 noise, integer phase
/// accumulators, Int16 WAV, byte-identical on every machine. `steno dev
/// fixtures generate` writes these files; `FixtureManifestTests` regenerates
/// them and compares with `Tests/Fixtures/MANIFEST.sha256`. Other workstreams
/// add cases here.
public enum FixtureGenerator {
  public struct Output: Sendable, Equatable, Hashable {
    public var relativePath: String
    public var sha256: String

    public init(relativePath: String, sha256: String) {
      self.relativePath = relativePath
      self.sha256 = sha256
    }
  }

  public static let sampleRate = WAVWriter.sampleRate

  /// Speaker A of the two-tone conversation: fundamental plus one harmonic.
  static let voiceA: [(frequency: Double, amplitude: Double)] = [(330, 0.35), (660, 0.15)]
  /// Speaker B.
  static let voiceB: [(frequency: Double, amplitude: Double)] = [(220, 0.35), (880, 0.15)]

  /// Every case in path order.
  public static var cases: [(relativePath: String, samples: () -> [Int16])] {
    [
      ("audio/conversation-mic-6s.wav", { conversation(seconds: 6, lanes: [.mic]) }),
      ("audio/conversation-system-6s.wav", { conversation(seconds: 6, lanes: [.system]) }),
      ("audio/conversation-two-lane-6s.wav", { conversation(seconds: 6, lanes: [.mic, .system]) }),
      ("audio/noise-2s.wav", { noise(seconds: 2, seed: 0x5EED_0001, amplitude: 0.2) }),
      ("audio/sweep-3s.wav", { sweep(seconds: 3, from: 200, to: 4000, amplitude: 0.5) }),
    ]
  }

  /// Writes every case under `root` and returns the manifest entries.
  @discardableResult
  public static func generate(into root: URL) throws -> [Output] {
    var outputs: [Output] = []
    for entry in cases {
      let url = root.appendingPathComponent(entry.relativePath)
      try FileManager.default.createDirectory(
        at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
      let data = WAVWriter.data(entry.samples())
      try data.write(to: url, options: .atomic)
      outputs.append(Output(relativePath: entry.relativePath, sha256: ContentHash.sha256Hex(data)))
    }
    return outputs.sorted { $0.relativePath < $1.relativePath }
  }

  /// `sha256sum` format: hash, two spaces, path.
  public static func manifest(_ outputs: [Output]) -> String {
    outputs.map { "\($0.sha256)  \($0.relativePath)" }.joined(separator: "\n") + "\n"
  }

  // MARK: - Signals

  /// A linear sine sweep.
  public static func sweep(seconds: Double, from start: Double, to end: Double, amplitude: Double)
    -> [Int16]
  {
    let count = Int(seconds * Double(sampleRate))
    var phase: UInt32 = 0
    var samples = [Int16](repeating: 0, count: count)
    for index in 0..<count {
      let progress = Double(index) / Double(max(count - 1, 1))
      let frequency = start + (end - start) * progress
      samples[index] = quantize(amplitude * sine(phase))
      phase = phase &+ increment(frequency)
    }
    return samples
  }

  /// Seeded white noise.
  public static func noise(seconds: Double, seed: UInt64, amplitude: Double) -> [Int16] {
    var generator = SplitMix64(seed: seed)
    let count = Int(seconds * Double(sampleRate))
    return (0..<count).map { _ in
      let unit = Double(generator.next() >> 11) / Double(1 << 53)  // [0, 1)
      return quantize(amplitude * (unit * 2 - 1))
    }
  }

  /// Speaker A and B alternate in 1.5 s turns with 0.1 s of silence between
  /// them; A speaks on `.mic`, B on `.system`. Passing one lane yields that
  /// lane's sidecar, both lanes the mixed room recording.
  public static func conversation(seconds: Double, lanes: [AudioLane]) -> [Int16] {
    let count = Int(seconds * Double(sampleRate))
    let turn = Int(1.5 * Double(sampleRate))
    let gap = Int(0.1 * Double(sampleRate))
    var phasesA = [UInt32](repeating: 0, count: voiceA.count)
    var phasesB = [UInt32](repeating: 0, count: voiceB.count)
    var samples = [Int16](repeating: 0, count: count)
    let includeA = lanes.contains(.mic) || lanes.contains(.mixed)
    let includeB = lanes.contains(.system) || lanes.contains(.mixed)
    for index in 0..<count {
      let turnIndex = index / turn
      let inGap = index % turn >= turn - gap
      var value = 0.0
      if !inGap {
        if turnIndex % 2 == 0, includeA {
          for (voice, partial) in voiceA.enumerated() {
            value += partial.amplitude * sine(phasesA[voice])
          }
        } else if turnIndex % 2 == 1, includeB {
          for (voice, partial) in voiceB.enumerated() {
            value += partial.amplitude * sine(phasesB[voice])
          }
        }
      }
      for (voice, partial) in voiceA.enumerated() {
        phasesA[voice] = phasesA[voice] &+ increment(partial.frequency)
      }
      for (voice, partial) in voiceB.enumerated() {
        phasesB[voice] = phasesB[voice] &+ increment(partial.frequency)
      }
      samples[index] = quantize(value)
    }
    return samples
  }

  static func increment(_ frequency: Double) -> UInt32 {
    UInt32((frequency / Double(sampleRate) * 4_294_967_296.0).rounded())
  }

  static func sine(_ phase: UInt32) -> Double {
    Foundation.sin(2 * Double.pi * Double(phase) / 4_294_967_296.0)
  }

  static func quantize(_ value: Double) -> Int16 {
    Int16(clamping: Int((min(1, max(-1, value)) * 32767).rounded()))
  }
}

/// The SplitMix64 generator: tiny, seedable, identical on every platform.
public struct SplitMix64: RandomNumberGenerator, Sendable {
  private var state: UInt64

  public init(seed: UInt64) {
    state = seed
  }

  public mutating func next() -> UInt64 {
    state = state &+ 0x9e37_79b9_7f4a_7c15
    var z = state
    z = (z ^ (z >> 30)) &* 0xbf58_476d_1ce4_e5b9
    z = (z ^ (z >> 27)) &* 0x94d0_49bb_1331_11eb
    return z ^ (z >> 31)
  }
}
