import Crypto
import Dispatch
import Foundation

/// The partial file of one recording: chunks land at `index * chunkSize`,
/// so the file is sparse until the last chunk arrives and the phone may send
/// chunks in any order or twice. Whole-file hashing streams in 1 MiB reads.
///
/// Every read and write runs on `io`, a concurrent queue of its own, never on
/// the engine actor or the cooperative pool: a chunk's fsync or the hash of
/// a 4 GiB file would otherwise hold every other request, including the auth
/// gate of unrelated connections and `/v1/hello`.
enum ReceivingFile {
  static let readBlock = 1024 * 1024

  private static let io = DispatchQueue(
    label: "steno.handover.receiving-file", qos: .utility, attributes: .concurrent)

  /// Creates an empty partial file (or leaves an existing one alone).
  static func create(at url: URL) throws {
    if !FileManager.default.fileExists(atPath: url.path) {
      guard FileManager.default.createFile(atPath: url.path, contents: nil) else {
        throw CocoaError(.fileWriteUnknown, userInfo: [NSFilePathErrorKey: url.path])
      }
    }
  }

  /// Writes `data` at `offset` and flushes it to disk.
  static func write(_ data: Data, at offset: UInt64, to url: URL) async throws {
    try await offActor {
      let handle = try FileHandle(forWritingTo: url)
      defer { try? handle.close() }
      try handle.seek(toOffset: offset)
      try handle.write(contentsOf: data)
      try handle.synchronize()
    }
  }

  /// Whether the whole file, streamed through SHA-256, hashes to `expected`.
  /// swift-crypto compares the digest in constant time.
  static func hashMatches(_ url: URL, expected: Data) async throws -> Bool {
    try await offActor {
      let handle = try FileHandle(forReadingFrom: url)
      defer { try? handle.close() }
      var hasher = SHA256()
      while let block = try handle.read(upToCount: readBlock), !block.isEmpty {
        hasher.update(data: block)
      }
      return hasher.finalize() == expected
    }
  }

  static func size(of url: URL) throws -> Int64 {
    let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
    return (attributes[.size] as? NSNumber)?.int64Value ?? 0
  }

  private static func offActor<T: Sendable>(_ body: @escaping @Sendable () throws -> T)
    async throws -> T
  {
    try await withCheckedThrowingContinuation { continuation in
      io.async { continuation.resume(with: Result(catching: body)) }
    }
  }
}
