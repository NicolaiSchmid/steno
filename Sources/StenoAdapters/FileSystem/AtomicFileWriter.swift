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
  struct Failure: Error, Sendable {
    var path: String
    var underlying: String
  }

  /// Temp files of the writer; the only files a destination ever removes.
  static let temporaryPrefix = ".steno-tmp-"

  static func write(_ data: Data, to url: URL) throws {
    let temporary = temporaryURL(for: url)
    do {
      try writeBytes(data, to: temporary.path, reportedAs: url.path)
      guard rename(temporary.path, url.path) == 0 else {
        throw Failure(path: url.path, underlying: "rename: \(errnoText())")
      }
    } catch {
      unlink(temporary.path)
      throw error
    }
  }

  /// Removes every `.steno-tmp-*` left in `directory` by an earlier crash.
  /// Nothing else is ever removed.
  static func removeStaleTemporaries(in directory: URL) {
    guard let names = try? FileManager.default.contentsOfDirectory(atPath: directory.path) else {
      return
    }
    for name in names where name.hasPrefix(temporaryPrefix) {
      try? FileManager.default.removeItem(at: directory.appendingPathComponent(name))
    }
  }

  static func temporaryURL(for url: URL) -> URL {
    url.deletingLastPathComponent()
      .appendingPathComponent("\(temporaryPrefix)\(url.lastPathComponent)-\(randomHex())")
  }

  /// Eight lowercase hex digits.
  static func randomHex() -> String {
    String(UUID().uuidString.prefix(8)).lowercased()
  }

  /// `open`, `write` until every byte is out, `fsync`; failures name the
  /// target the caller asked for, not the temp file.
  private static func writeBytes(_ data: Data, to path: String, reportedAs target: String) throws {
    let descriptor = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0o644)
    guard descriptor >= 0 else {
      throw Failure(path: target, underlying: "open: \(errnoText())")
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
          throw Failure(path: target, underlying: "write: \(errnoText())")
        }
        offset += Int(written)
      }
    }
    guard fsync(descriptor) == 0 else {
      throw Failure(path: target, underlying: "fsync: \(errnoText())")
    }
  }

  private static func errnoText() -> String {
    String(cString: strerror(errno))
  }
}
