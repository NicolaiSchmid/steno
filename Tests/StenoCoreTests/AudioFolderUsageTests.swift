import Foundation
import Testing

@testable import StenoCore

@Suite struct AudioFolderUsageTests {
  private func write(_ bytes: Int, at url: URL) throws {
    try FileManager.default.createDirectory(
      at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
    try Data(repeating: 7, count: bytes).write(to: url)
  }

  @Test func sumsLogicalSizesRecursivelyAndSkipsHiddenFiles() throws {
    let directory = try Fixtures.temporaryDirectory("usage")
    defer { try? FileManager.default.removeItem(at: directory) }
    try write(1_000, at: directory.appendingPathComponent("master.caf"))
    try write(200, at: directory.appendingPathComponent("meeting/mic.wav"))
    try write(34, at: directory.appendingPathComponent("meeting/speakers/clip.wav"))
    try write(0, at: directory.appendingPathComponent("meeting/empty.m4a"))
    try write(50, at: directory.appendingPathComponent(".DS_Store"))
    try write(9, at: directory.appendingPathComponent("meeting/.hidden"))
    #expect(try AudioFolderUsage.measure(directory) == 1_234)
    #expect(throws: (any Error).self, "a file is not a folder") {
      try AudioFolderUsage.measure(directory.appendingPathComponent("master.caf"))
    }
  }

  /// A folder that exists but cannot be read throws, so Settings says
  /// "Size unavailable" rather than a zero that looks like an empty folder.
  @Test func anUnreadableFolderThrows() throws {
    // Root reads everything; the permission bits cannot make a folder
    // unreadable for it.
    guard getuid() != 0 else { return }
    let directory = try Fixtures.temporaryDirectory("usage-unreadable")
    defer {
      try? FileManager.default.setAttributes(
        [.posixPermissions: 0o700], ofItemAtPath: directory.path)
      try? FileManager.default.removeItem(at: directory)
    }
    try write(10, at: directory.appendingPathComponent("master.caf"))
    try FileManager.default.setAttributes([.posixPermissions: 0], ofItemAtPath: directory.path)
    #expect(throws: (any Error).self) { try AudioFolderUsage.measure(directory) }
  }

  @Test func anEmptyFolderIsZero() throws {
    let directory = try Fixtures.temporaryDirectory("usage-empty")
    defer { try? FileManager.default.removeItem(at: directory) }
    #expect(try AudioFolderUsage.measure(directory) == 0)
  }

  @Test func aMissingFolderThrows() throws {
    let missing = FileManager.default.temporaryDirectory
      .appendingPathComponent("steno-usage-missing-\(UUID().uuidString)", isDirectory: true)
    #expect(throws: (any Error).self) { try AudioFolderUsage.measure(missing) }
  }
}
