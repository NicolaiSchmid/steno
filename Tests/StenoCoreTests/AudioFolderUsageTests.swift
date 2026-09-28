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
