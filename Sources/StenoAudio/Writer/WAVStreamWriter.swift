import Foundation

/// Streams 16 kHz mono Int16 PCM into a RIFF/WAVE file: header with zero
/// sizes first, samples appended per frame, sizes patched by `finish()`. The
/// sidecar format `WAVAudioDecoder` reads. A file whose process died before
/// `finish` has zero sizes; the master CAF is the recoverable copy, and the
/// decoder rebuilds the lane from it.
public final class WAVStreamWriter: @unchecked Sendable {
  public let url: URL
  public let sampleRate: Int
  public private(set) var samplesWritten = 0
  private let handle: FileHandle
  private var isFinished = false

  public init(url: URL, sampleRate: Int = 16_000) throws {
    self.url = url
    self.sampleRate = sampleRate
    _ = FileManager.default.createFile(atPath: url.path, contents: nil)
    handle = try FileHandle(forWritingTo: url)
    try handle.truncate(atOffset: 0)
    try handle.write(contentsOf: Self.header(sampleRate: sampleRate, sampleCount: 0))
  }

  public func write(_ samples: UnsafePointer<Int16>, count: Int) throws {
    guard count > 0, !isFinished else { return }
    let data = Data(
      bytesNoCopy: UnsafeMutableRawPointer(mutating: samples), count: count * 2, deallocator: .none
    )
    try handle.write(contentsOf: data)
    samplesWritten += count
  }

  public func finish() throws {
    guard !isFinished else { return }
    isFinished = true
    try handle.seek(toOffset: 0)
    try handle.write(contentsOf: Self.header(sampleRate: sampleRate, sampleCount: samplesWritten))
    try handle.synchronize()
    try handle.close()
  }

  public var duration: TimeInterval { Double(samplesWritten) / Double(sampleRate) }

  /// The 44-byte RIFF, `fmt ` and `data` headers for a 16-bit mono file.
  static func header(sampleRate: Int, sampleCount: Int) -> Data {
    let dataSize = sampleCount * 2
    var data = Data(capacity: 44)
    data.append(contentsOf: Array("RIFF".utf8))
    append(UInt32(36 + dataSize), to: &data)
    data.append(contentsOf: Array("WAVE".utf8))
    data.append(contentsOf: Array("fmt ".utf8))
    append(UInt32(16), to: &data)
    append(UInt16(1), to: &data)
    append(UInt16(1), to: &data)
    append(UInt32(sampleRate), to: &data)
    append(UInt32(sampleRate * 2), to: &data)
    append(UInt16(2), to: &data)
    append(UInt16(16), to: &data)
    data.append(contentsOf: Array("data".utf8))
    append(UInt32(dataSize), to: &data)
    return data
  }

  private static func append<T: FixedWidthInteger>(_ value: T, to data: inout Data) {
    withUnsafeBytes(of: value.littleEndian) { data.append(contentsOf: $0) }
  }
}
