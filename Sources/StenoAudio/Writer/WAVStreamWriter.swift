import Foundation

/// Streams 16 kHz mono Int16 PCM into a RIFF/WAVE file: header with zero
/// sizes first, samples appended per frame, sizes patched by `finish()`. The
/// sidecar format `WAVAudioDecoder` reads. A file whose process died before
/// `finish` has zero sizes; the master CAF is the recoverable copy, and the
/// decoder rebuilds the lane from it.
final class WAVStreamWriter: @unchecked Sendable {
  /// `RIFF` size + `WAVE`, the 16-byte `fmt ` chunk with its header, and the
  /// `data` chunk header: 44 bytes before the first sample.
  static let headerSize = 44
  /// The RIFF size field counts everything after itself: the header minus
  /// the 8-byte `RIFF` chunk header, plus the samples.
  static let riffSizeBeforeData = headerSize - 8
  static let bytesPerSample = 2

  let url: URL
  let sampleRate: Int
  private(set) var samplesWritten = 0
  private let handle: FileHandle
  private var isFinished = false

  init(url: URL, sampleRate: Int = 16_000) throws {
    self.url = url
    self.sampleRate = sampleRate
    _ = FileManager.default.createFile(atPath: url.path, contents: nil)
    handle = try FileHandle(forWritingTo: url)
    try handle.truncate(atOffset: 0)
    try handle.write(contentsOf: Self.header(sampleRate: sampleRate, sampleCount: 0))
  }

  func write(_ samples: UnsafePointer<Int16>, count: Int) throws {
    guard count > 0, !isFinished else { return }
    let data = Data(
      bytesNoCopy: UnsafeMutableRawPointer(mutating: samples), count: count * Self.bytesPerSample,
      deallocator: .none)
    try handle.write(contentsOf: data)
    samplesWritten += count
  }

  func finish() throws {
    guard !isFinished else { return }
    isFinished = true
    try handle.seek(toOffset: 0)
    try handle.write(contentsOf: Self.header(sampleRate: sampleRate, sampleCount: samplesWritten))
    try handle.synchronize()
    try handle.close()
  }

  var duration: TimeInterval { Double(samplesWritten) / Double(sampleRate) }

  /// The 44-byte RIFF, `fmt ` and `data` headers for a 16-bit mono file.
  static func header(sampleRate: Int, sampleCount: Int) -> Data {
    let dataSize = sampleCount * bytesPerSample
    var data = Data(capacity: headerSize)
    data.append(contentsOf: Array("RIFF".utf8))
    append(UInt32(riffSizeBeforeData + dataSize), to: &data)
    data.append(contentsOf: Array("WAVE".utf8))
    data.append(contentsOf: Array("fmt ".utf8))
    append(UInt32(16), to: &data)  // fmt chunk size
    append(UInt16(1), to: &data)  // PCM
    append(UInt16(1), to: &data)  // mono
    append(UInt32(sampleRate), to: &data)
    append(UInt32(sampleRate * bytesPerSample), to: &data)  // bytes per second
    append(UInt16(bytesPerSample), to: &data)  // block align
    append(UInt16(bytesPerSample * 8), to: &data)  // bits per sample
    data.append(contentsOf: Array("data".utf8))
    append(UInt32(dataSize), to: &data)
    return data
  }

  private static func append<T: FixedWidthInteger>(_ value: T, to data: inout Data) {
    withUnsafeBytes(of: value.littleEndian) { data.append(contentsOf: $0) }
  }
}
