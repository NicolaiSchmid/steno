import Foundation

#if canImport(Darwin)
  import Darwin
#else
  import Glibc
#endif

/// Writes a file so a reader never sees a half-written one: the bytes go to
/// `.steno-tmp-<name>-<8 hex>` in the target directory, are `fsync`ed, and
/// `rename(2)` replaces the target in one step. A failure removes the temp
/// file and leaves the target as it was.
enum AtomicFileWriter {
  struct Failure: Error, Sendable, Equatable, CustomStringConvertible {
    var path: String
    var underlying: String
    var description: String { "could not write \(path): \(underlying)" }
  }

  static func write(_ data: Data, to url: URL) throws {
    let temporary = temporaryURL(for: url)
    do {
      try writeBytes(data, to: temporary.path)
      guard rename(temporary.path, url.path) == 0 else {
        throw Failure(path: url.path, underlying: "rename: \(errnoText())")
      }
    } catch {
      unlink(temporary.path)
      if let failure = error as? Failure {
        throw Failure(path: url.path, underlying: failure.underlying)
      }
      throw Failure(path: url.path, underlying: String(describing: error))
    }
  }

  /// Removes every `.steno-tmp-*` left in `directory` by an earlier crash.
  /// Nothing else is ever removed.
  static func removeStaleTemporaries(in directory: URL) {
    guard let names = try? FileManager.default.contentsOfDirectory(atPath: directory.path) else {
      return
    }
    for name in names where name.hasPrefix(ObsidianLayout.temporaryPrefix) {
      try? FileManager.default.removeItem(at: directory.appendingPathComponent(name))
    }
  }

  static func temporaryURL(for url: URL) -> URL {
    url.deletingLastPathComponent()
      .appendingPathComponent(
        "\(ObsidianLayout.temporaryPrefix)\(url.lastPathComponent)-\(randomHex())")
  }

  /// Eight lowercase hex digits.
  static func randomHex() -> String {
    let hex = String(UInt32.random(in: 0...UInt32.max), radix: 16)
    return String(repeating: "0", count: max(0, 8 - hex.count)) + hex
  }

  private static func writeBytes(_ data: Data, to path: String) throws {
    let descriptor = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0o644)
    guard descriptor >= 0 else {
      throw Failure(path: path, underlying: "open: \(errnoText())")
    }
    defer { close(descriptor) }
    var offset = 0
    let count = data.count
    try data.withUnsafeBytes { (buffer: UnsafeRawBufferPointer) in
      guard let base = buffer.baseAddress else { return }
      while offset < count {
        let written = Foundation.write(descriptor, base + offset, count - offset)
        if written < 0 {
          if errno == EINTR { continue }
          throw Failure(path: path, underlying: "write: \(errnoText())")
        }
        offset += Int(written)
      }
    }
    guard fsync(descriptor) == 0 else {
      throw Failure(path: path, underlying: "fsync: \(errnoText())")
    }
  }

  private static func errnoText() -> String {
    String(cString: strerror(errno))
  }
}
