import Foundation

/// Writes 16 kHz mono 16-bit PCM WAV: sample clips, fixtures and the
/// `steno dev fixtures generate` output.
public enum WAVWriter {
  public static let sampleRate = Int(AudioBuffer16k.sampleRate)

  /// Clamps to `-1...1`, scales to Int16 and writes.
  public static func write(_ buffer: AudioBuffer16k, to url: URL) throws {
    try write(int16(buffer.samples), to: url)
  }

  public static func write(_ samples: [Int16], to url: URL) throws {
    try data(samples).write(to: url, options: .atomic)
  }

  /// The complete file for `samples`. `sampleRate` and `channels` other than
  /// 16 000 and 1 exist so tests can build files the decoder must reject.
  public static func data(_ samples: [Int16], sampleRate: Int = sampleRate, channels: Int = 1)
    -> Data
  {
    let bytesPerSample = 2
    let dataSize = samples.count * bytesPerSample
    var data = Data(capacity: 44 + dataSize)
    data.append(contentsOf: Array("RIFF".utf8))
    append(UInt32(36 + dataSize), to: &data)
    data.append(contentsOf: Array("WAVE".utf8))
    data.append(contentsOf: Array("fmt ".utf8))
    append(UInt32(16), to: &data)
    append(UInt16(1), to: &data)
    append(UInt16(channels), to: &data)
    append(UInt32(sampleRate), to: &data)
    append(UInt32(sampleRate * channels * bytesPerSample), to: &data)
    append(UInt16(channels * bytesPerSample), to: &data)
    append(UInt16(16), to: &data)
    data.append(contentsOf: Array("data".utf8))
    append(UInt32(dataSize), to: &data)
    for sample in samples {
      append(UInt16(bitPattern: sample), to: &data)
    }
    return data
  }

  /// A 32-bit float file, for decoder tests.
  public static func float32Data(_ samples: [Float]) -> Data {
    let dataSize = samples.count * 4
    var data = Data(capacity: 44 + dataSize)
    data.append(contentsOf: Array("RIFF".utf8))
    append(UInt32(36 + dataSize), to: &data)
    data.append(contentsOf: Array("WAVE".utf8))
    data.append(contentsOf: Array("fmt ".utf8))
    append(UInt32(16), to: &data)
    append(UInt16(3), to: &data)
    append(UInt16(1), to: &data)
    append(UInt32(sampleRate), to: &data)
    append(UInt32(sampleRate * 4), to: &data)
    append(UInt16(4), to: &data)
    append(UInt16(32), to: &data)
    data.append(contentsOf: Array("data".utf8))
    append(UInt32(dataSize), to: &data)
    for sample in samples {
      append(sample.bitPattern, to: &data)
    }
    return data
  }

  public static func int16(_ samples: [Float]) -> [Int16] {
    samples.map { sample in
      let clamped = min(1, max(-1, sample))
      return Int16(clamping: Int((clamped * 32767).rounded()))
    }
  }

  private static func append<T: FixedWidthInteger>(_ value: T, to data: inout Data) {
    withUnsafeBytes(of: value.littleEndian) { data.append(contentsOf: $0) }
  }
}
