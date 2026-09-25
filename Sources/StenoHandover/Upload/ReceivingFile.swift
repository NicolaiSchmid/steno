import Crypto
import Foundation

/// The partial file of one recording: chunks land at `index * chunkSize`,
/// so the file is sparse until the last chunk arrives and the phone may send
/// chunks in any order or twice. Whole-file hashing streams in 1 MiB reads.
enum ReceivingFile {
  static let readBlock = 1024 * 1024

  /// Creates an empty partial file (or leaves an existing one alone).
  static func create(at url: URL) throws {
    try FileManager.default.createDirectory(
      at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
    if !FileManager.default.fileExists(atPath: url.path) {
      guard FileManager.default.createFile(atPath: url.path, contents: nil) else {
        throw UploadError.io("could not create \(url.lastPathComponent)")
      }
    }
  }

  /// Writes `data` at `offset` and flushes it to disk.
  static func write(_ data: Data, at offset: UInt64, to url: URL) throws {
    let handle = try FileHandle(forWritingTo: url)
    defer { try? handle.close() }
    try handle.seek(toOffset: offset)
    try handle.write(contentsOf: data)
    try handle.synchronize()
  }

  /// SHA-256 of the whole file, streamed.
  static func sha256(of url: URL) throws -> Data {
    let handle = try FileHandle(forReadingFrom: url)
    defer { try? handle.close() }
    var hasher = SHA256()
    while let block = try handle.read(upToCount: readBlock), !block.isEmpty {
      hasher.update(data: block)
    }
    return Data(hasher.finalize())
  }

  static func size(of url: URL) throws -> Int64 {
    let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
    return (attributes[.size] as? NSNumber)?.int64Value ?? 0
  }

  static func sha256(_ data: Data) -> Data {
    Data(SHA256.hash(data: data))
  }
}

enum UploadError: Error, CustomStringConvertible, Sendable {
  case io(String)

  var description: String {
    switch self {
    case .io(let message): message
    }
  }
}
