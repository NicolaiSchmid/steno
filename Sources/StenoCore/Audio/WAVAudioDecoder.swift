import Foundation

public enum WAVDecodeError: Error, Sendable, Equatable, CustomStringConvertible {
  /// Not 16 kHz mono 16-bit integer or 32-bit float PCM.
  case unsupportedFormat(String)
  /// Not a RIFF/WAVE file, or a chunk is truncated.
  case malformed(String)

  public var description: String {
    switch self {
    case .unsupportedFormat(let detail): "unsupported WAV format: \(detail)"
    case .malformed(let detail): "malformed WAV file: \(detail)"
    }
  }
}

/// A RIFF/WAVE reader for fixtures, sample clips and `steno process` input:
/// 16 kHz mono only, 16-bit integer or 32-bit float PCM. The real
/// `AudioDecoder` (CAF, m4a, resampling) is StenoAudio's.
public struct WAVAudioDecoder: AudioDecoder, Sendable {
  public struct Info: Sendable, Equatable {
    public var sampleRate: Int
    public var channels: Int
    public var bitsPerSample: Int
    public var isFloat: Bool
    public var frameCount: Int
  }

  public init() {}

  /// The lane's sidecar when present, else the master; both must be WAV.
  public func decode(_ asset: AudioAsset, lane: AudioLane) async throws -> AudioBuffer16k {
    try Self.read(asset.sidecars16k[lane] ?? asset.url)
  }

  /// Copies the master; there is no AAC encoder in core.
  public func mixdown(_ asset: AudioAsset, to url: URL) async throws {
    try FileManager.default.createDirectory(
      at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
    if FileManager.default.fileExists(atPath: url.path) {
      try FileManager.default.removeItem(at: url)
    }
    try FileManager.default.copyItem(at: asset.url, to: url)
  }

  public static func read(_ url: URL) throws -> AudioBuffer16k {
    try read(Data(contentsOf: url))
  }

  public static func read(_ data: Data) throws -> AudioBuffer16k {
    let (info, samples) = try parse(data)
    guard info.sampleRate == Int(AudioBuffer16k.sampleRate), info.channels == 1 else {
      throw WAVDecodeError.unsupportedFormat(
        "\(info.sampleRate) Hz, \(info.channels) channel(s); need 16000 Hz mono")
    }
    let count = info.frameCount
    var floats = [Float](repeating: 0, count: count)
    if info.isFloat {
      guard info.bitsPerSample == 32 else {
        throw WAVDecodeError.unsupportedFormat("\(info.bitsPerSample)-bit float")
      }
      samples.withUnsafeBytes { raw in
        for index in 0..<count {
          let bits = raw.loadUnaligned(fromByteOffset: index * 4, as: UInt32.self)
          floats[index] = Float(bitPattern: UInt32(littleEndian: bits))
        }
      }
    } else {
      guard info.bitsPerSample == 16 else {
        throw WAVDecodeError.unsupportedFormat("\(info.bitsPerSample)-bit integer")
      }
      samples.withUnsafeBytes { raw in
        for index in 0..<count {
          let bits = raw.loadUnaligned(fromByteOffset: index * 2, as: UInt16.self)
          floats[index] = Float(Int16(bitPattern: UInt16(littleEndian: bits))) / 32768
        }
      }
    }
    return AudioBuffer16k(samples: floats)
  }

  public static func info(_ url: URL) throws -> Info {
    try parse(Data(contentsOf: url)).info
  }

  /// Walks the RIFF chunks and returns the format plus the raw `data` chunk.
  static func parse(_ data: Data) throws -> (info: Info, samples: Data) {
    guard data.count >= 12 else { throw WAVDecodeError.malformed("shorter than a RIFF header") }
    guard tag(data, at: 0) == "RIFF", tag(data, at: 8) == "WAVE" else {
      throw WAVDecodeError.malformed("missing RIFF/WAVE tags")
    }
    var offset = 12
    var format: (audioFormat: UInt16, channels: Int, sampleRate: Int, bits: Int)?
    var samples: Data?
    while offset + 8 <= data.count {
      let id = tag(data, at: offset)
      let size = Int(uint32(data, at: offset + 4))
      let body = offset + 8
      guard body + size <= data.count else {
        throw WAVDecodeError.malformed("chunk \(id) runs past the end of the file")
      }
      switch id {
      case "fmt ":
        guard size >= 16 else { throw WAVDecodeError.malformed("fmt chunk too short") }
        var audioFormat = uint16(data, at: body)
        if audioFormat == 0xFFFE, size >= 26 {
          audioFormat = uint16(data, at: body + 24)
        }
        format = (
          audioFormat, Int(uint16(data, at: body + 2)), Int(uint32(data, at: body + 4)),
          Int(uint16(data, at: body + 14))
        )
      case "data":
        samples = data.subdata(in: body..<(body + size))
      default:
        break
      }
      offset = body + size + (size % 2)
    }
    guard let format else { throw WAVDecodeError.malformed("no fmt chunk") }
    guard let samples else { throw WAVDecodeError.malformed("no data chunk") }
    let isFloat: Bool
    switch format.audioFormat {
    case 1: isFloat = false
    case 3: isFloat = true
    default: throw WAVDecodeError.unsupportedFormat("audio format tag \(format.audioFormat)")
    }
    let bytesPerFrame = max(1, format.channels * format.bits / 8)
    let info = Info(
      sampleRate: format.sampleRate, channels: format.channels, bitsPerSample: format.bits,
      isFloat: isFloat, frameCount: samples.count / bytesPerFrame)
    return (info, samples)
  }

  private static func tag(_ data: Data, at offset: Int) -> String {
    String(decoding: data.subdata(in: offset..<(offset + 4)), as: UTF8.self)
  }

  private static func uint16(_ data: Data, at offset: Int) -> UInt16 {
    UInt16(data[data.startIndex + offset]) | UInt16(data[data.startIndex + offset + 1]) << 8
  }

  private static func uint32(_ data: Data, at offset: Int) -> UInt32 {
    UInt32(uint16(data, at: offset)) | UInt32(uint16(data, at: offset + 2)) << 16
  }
}
