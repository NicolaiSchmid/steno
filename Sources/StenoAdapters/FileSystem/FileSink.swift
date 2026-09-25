import Foundation

/// Where a destination's bytes land, addressed by paths relative to a root.
/// One implementation today (`LocalFolderSink`); a WebDAV sink would be the
/// second, at which point this goes public per the two-implementations rule.
protocol FileSink: Sendable {
  var root: URL { get }
  func exists(_ relativePath: String) -> Bool
  func isDirectory(_ relativePath: String) -> Bool
  /// nil when there is no such file.
  func read(_ relativePath: String) throws -> Data?
  func write(_ data: Data, to relativePath: String) throws
  func createDirectory(_ relativePath: String) throws
  func removeStaleTemporaries(in relativeDirectory: String)
}

/// The local file system under one folder, writing through
/// `AtomicFileWriter`.
struct LocalFolderSink: FileSink {
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

  func removeStaleTemporaries(in relativeDirectory: String) {
    AtomicFileWriter.removeStaleTemporaries(in: url(relativeDirectory))
  }
}
