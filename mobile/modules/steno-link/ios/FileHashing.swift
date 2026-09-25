import CryptoKit
import Foundation

/// Streaming SHA-256 over files and file slices. Reads in 1 MiB steps so a
/// 30 MB recording never sits in memory at once, and never crosses the JS
/// bridge as bytes.
enum FileHashing {
  static let bufferSize = 1 << 20

  /// SHA-256 of the whole file.
  static func sha256(fileAt url: URL) throws -> Data {
    var hasher = SHA256()
    try forEachBlock(of: url, offset: 0, length: nil) { block in
      hasher.update(data: block)
    }
    return Data(hasher.finalize())
  }

  /// Copies `[offset, offset + length)` of `source` to `destination`
  /// (replacing it) and returns the slice's SHA-256 computed on the way.
  static func copySlice(of source: URL, offset: UInt64, length: UInt64, to destination: URL) throws -> Data {
    let manager = FileManager.default
    if manager.fileExists(atPath: destination.path) {
      try manager.removeItem(at: destination)
    }
    guard manager.createFile(atPath: destination.path, contents: nil) else {
      throw FileHashingError.cannotCreate(destination.path)
    }
    let writer = try FileHandle(forWritingTo: destination)
    defer { try? writer.close() }
    var hasher = SHA256()
    try forEachBlock(of: source, offset: offset, length: length) { block in
      hasher.update(data: block)
      try writer.write(contentsOf: block)
    }
    return Data(hasher.finalize())
  }

  private static func forEachBlock(
    of url: URL,
    offset: UInt64,
    length: UInt64?,
    _ body: (Data) throws -> Void
  ) throws {
    let handle = try FileHandle(forReadingFrom: url)
    defer { try? handle.close() }
    try handle.seek(toOffset: offset)
    var remaining = length ?? UInt64.max
    while remaining > 0 {
      let want = Int(min(UInt64(bufferSize), remaining))
      guard let block = try handle.read(upToCount: want), !block.isEmpty else { break }
      try body(block)
      remaining -= UInt64(block.count)
    }
    if length != nil, remaining > 0 {
      throw FileHashingError.shortRead
    }
  }
}

enum FileHashingError: LocalizedError {
  case shortRead
  case cannotCreate(String)

  var errorDescription: String? {
    switch self {
    case .shortRead: return "File ended before the requested length"
    case .cannotCreate(let path): return "Could not create \(path)"
    }
  }
}
