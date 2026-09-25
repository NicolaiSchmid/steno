import Foundation

/// The local file system under one folder, addressed by paths relative to
/// its root and written through `AtomicFileWriter`. The destination's one
/// seam to disk; a WebDAV sink would be the second implementation, at which
/// point a protocol is extracted per the two-implementations rule.
struct LocalFolderSink: Sendable {
  let root: URL

  func url(_ relativePath: String) -> URL {
    relativePath.isEmpty ? root : root.appendingPathComponent(relativePath)
  }

  func exists(_ relativePath: String) -> Bool {
    FileManager.default.fileExists(atPath: url(relativePath).path)
  }

  func isDirectory(_ relativePath: String) -> Bool {
    var isDirectory: ObjCBool = false
    return FileManager.default.fileExists(atPath: url(relativePath).path, isDirectory: &isDirectory)
      && isDirectory.boolValue
  }

  /// nil when there is no such file.
  func read(_ relativePath: String) throws -> Data? {
    guard exists(relativePath) else { return nil }
    return try Data(contentsOf: url(relativePath))
  }

  func write(_ data: Data, to relativePath: String) throws {
    try AtomicFileWriter.write(data, to: url(relativePath))
  }

  func createDirectory(_ relativePath: String) throws {
    try FileManager.default.createDirectory(
      at: url(relativePath), withIntermediateDirectories: true)
  }
}
