import Foundation

/// A crash-tolerant CAF writer for Float32 little-endian PCM: `caff` header,
/// `desc` chunk, then a `data` chunk whose size is -1 ("to the end of the
/// file", the streaming form Core Audio's own writers use) while recording.
/// Every `write` appends whole frames, so a process killed mid-recording
/// leaves a file `AVAudioFile` reads up to the last frame written;
/// `finish()` patches the real size in. Big-endian chunk headers, as the CAF
/// specification requires; no AudioToolbox, so the writer and its tests run
/// on Linux too.
final class CAFStreamWriter: @unchecked Sendable {
  /// `caff`, version, flags.
  static let fileHeaderSize = 8
  /// Chunk type (4) and chunk size (8), before every chunk body.
  static let chunkHeaderSize = 12
  /// The `desc` chunk body: an `AudioStreamBasicDescription` without the
  /// reserved field.
  static let descChunkSize = 32
  /// The `data` chunk body starts with the edit count; the samples follow.
  static let editCountSize = 4
  /// `kCAFLinearPCMFormatFlagIsFloat | kCAFLinearPCMFormatFlagIsLittleEndian`.
  static let floatLittleEndianFlags: UInt32 = 0b11
  static let bytesPerSample = 4
  /// Everything before the first sample: 68 bytes.
  static let headerSize =
    fileHeaderSize + chunkHeaderSize + descChunkSize + chunkHeaderSize + editCountSize
  /// Where the `data` chunk's size field sits: after the type tag of the
  /// chunk that follows the `desc` chunk.
  static let dataSizeOffset = UInt64(fileHeaderSize + chunkHeaderSize + descChunkSize + 4)

  let url: URL
  let sampleRate: Double
  let channels: Int
  private(set) var framesWritten = 0
  private let handle: FileHandle
  private var isFinished = false

  init(url: URL, sampleRate: Double, channels: Int) throws {
    precondition(channels > 0)
    self.url = url
    self.sampleRate = sampleRate
    self.channels = channels
    _ = FileManager.default.createFile(atPath: url.path, contents: nil)
    handle = try FileHandle(forWritingTo: url)
    try handle.truncate(atOffset: 0)
    try handle.write(contentsOf: Self.header(sampleRate: sampleRate, channels: channels))
  }

  /// Appends `frameCount` interleaved frames.
  func write(interleaved: UnsafePointer<Float>, frameCount: Int) throws {
    guard frameCount > 0, !isFinished else { return }
    let byteCount = frameCount * channels * Self.bytesPerSample
    let data = Data(
      bytesNoCopy: UnsafeMutableRawPointer(mutating: interleaved), count: byteCount,
      deallocator: .none)
    try handle.write(contentsOf: data)
    framesWritten += frameCount
  }

  /// Patches the data chunk size (edit count plus samples), flushes and
  /// closes.
  func finish() throws {
    guard !isFinished else { return }
    isFinished = true
    var size = Int64(Self.editCountSize + framesWritten * channels * Self.bytesPerSample).bigEndian
    let patch = withUnsafeBytes(of: &size) { Data($0) }
    try handle.seek(toOffset: Self.dataSizeOffset)
    try handle.write(contentsOf: patch)
    try handle.synchronize()
    try handle.close()
  }

  var duration: TimeInterval { Double(framesWritten) / sampleRate }

  /// `caff` file header, `desc` chunk and the `data` chunk header with size
  /// -1 and edit count 0.
  static func header(sampleRate: Double, channels: Int) -> Data {
    var data = Data(capacity: headerSize)
    data.append(contentsOf: Array("caff".utf8))
    append(UInt16(1), to: &data)  // file version
    append(UInt16(0), to: &data)  // file flags
    data.append(contentsOf: Array("desc".utf8))
    append(Int64(descChunkSize), to: &data)
    append(sampleRate.bitPattern, to: &data)
    data.append(contentsOf: Array("lpcm".utf8))
    append(floatLittleEndianFlags, to: &data)
    append(UInt32(channels * bytesPerSample), to: &data)  // bytes per packet
    append(UInt32(1), to: &data)  // frames per packet
    append(UInt32(channels), to: &data)
    append(UInt32(bytesPerSample * 8), to: &data)  // bits per channel
    data.append(contentsOf: Array("data".utf8))
    append(Int64(-1), to: &data)  // size: to the end of the file, until finish()
    append(UInt32(0), to: &data)  // edit count
    assert(data.count == headerSize)
    return data
  }

  private static func append<T: FixedWidthInteger>(_ value: T, to data: inout Data) {
    withUnsafeBytes(of: value.bigEndian) { data.append(contentsOf: $0) }
  }
}

public enum CAFReadError: Error, Sendable, Equatable, CustomStringConvertible {
  case malformed(String)
  case unsupportedFormat(String)

  public var description: String {
    switch self {
    case .malformed(let detail): "malformed CAF file: \(detail)"
    case .unsupportedFormat(let detail): "unsupported CAF format: \(detail)"
    }
  }
}

/// Reads the CAF files the writer produces (Float32 little-endian PCM, any
/// channel count, data size -1 accepted) into de-interleaved channels. For
/// tests, `steno dev aec-bench`, `steno dev capture-spike` and crash
/// recovery; the pipeline's decoder is the AVFoundation codec.
public struct CAFFile: Sendable, Equatable {
  public var sampleRate: Double
  public var channels: [[Float]]

  public var frameCount: Int { channels.first?.count ?? 0 }
  public var duration: TimeInterval { Double(frameCount) / sampleRate }

  public static func read(_ url: URL) throws -> CAFFile {
    try read(Data(contentsOf: url))
  }

  public static func read(_ data: Data) throws -> CAFFile {
    guard data.count >= 8, tag(data, at: 0) == "caff" else {
      throw CAFReadError.malformed("missing caff header")
    }
    var offset = 8
    var format: (rate: Double, flags: UInt32, channels: Int, bits: Int, formatID: String)?
    var samples: (offset: Int, count: Int)?
    while offset + 12 <= data.count {
      let type = tag(data, at: offset)
      let size = Int64(bitPattern: uint64(data, at: offset + 4))
      let body = offset + 12
      switch type {
      case "desc":
        let descSize = CAFStreamWriter.descChunkSize
        guard size >= Int64(descSize), body + descSize <= data.count else {
          throw CAFReadError.malformed("desc chunk too short")
        }
        format = (
          Double(bitPattern: uint64(data, at: body)), uint32(data, at: body + 12),
          Int(uint32(data, at: body + 24)), Int(uint32(data, at: body + 28)),
          tag(data, at: body + 8)
        )
      case "data":
        let editCount = CAFStreamWriter.editCountSize
        guard body + editCount <= data.count else {
          throw CAFReadError.malformed("data chunk too short")
        }
        let available = data.count - body - editCount
        let count = size < 0 ? available : min(Int(size) - editCount, available)
        samples = (body + editCount, max(0, count))
      default:
        break
      }
      if size < 0 { break }
      offset = body + Int(size)
    }
    guard let format else { throw CAFReadError.malformed("no desc chunk") }
    guard let samples else { throw CAFReadError.malformed("no data chunk") }
    let flags = CAFStreamWriter.floatLittleEndianFlags
    let bitsPerSample = CAFStreamWriter.bytesPerSample * 8
    guard format.formatID == "lpcm", format.flags & flags == flags, format.bits == bitsPerSample
    else {
      throw CAFReadError.unsupportedFormat(
        "\(format.formatID) flags \(format.flags) \(format.bits)-bit; need Float32 little-endian")
    }
    let channelCount = max(1, format.channels)
    let bytesPerSample = CAFStreamWriter.bytesPerSample
    let frames = samples.count / (bytesPerSample * channelCount)
    var channels = [[Float]](repeating: [Float](repeating: 0, count: frames), count: channelCount)
    data.withUnsafeBytes { raw in
      for frame in 0..<frames {
        for channel in 0..<channelCount {
          let at = samples.offset + (frame * channelCount + channel) * bytesPerSample
          let bits = raw.loadUnaligned(fromByteOffset: at, as: UInt32.self)
          channels[channel][frame] = Float(bitPattern: UInt32(littleEndian: bits))
        }
      }
    }
    return CAFFile(sampleRate: format.rate, channels: channels)
  }

  private static func tag(_ data: Data, at offset: Int) -> String {
    String(
      decoding: data.subdata(in: (data.startIndex + offset)..<(data.startIndex + offset + 4)),
      as: UTF8.self)
  }

  private static func uint32(_ data: Data, at offset: Int) -> UInt32 {
    var value: UInt32 = 0
    for index in 0..<4 {
      value = value << 8 | UInt32(data[data.startIndex + offset + index])
    }
    return value
  }

  private static func uint64(_ data: Data, at offset: Int) -> UInt64 {
    var value: UInt64 = 0
    for index in 0..<8 {
      value = value << 8 | UInt64(data[data.startIndex + offset + index])
    }
    return value
  }
}
