import Foundation
import StenoCore
import Testing

@testable import StenoAdapters

@Suite struct AtomicFileWriterTests {
  @Test func writesTheBytesAndLeavesNoTemporaries() throws {
    let directory = try Fixtures.temporaryDirectory("atomic")
    defer { try? FileManager.default.removeItem(at: directory) }
    let target = directory.appendingPathComponent("note.md")
    try AtomicFileWriter.write(Data("one\n".utf8), to: target)
    #expect(try Data(contentsOf: target) == Data("one\n".utf8))
    try AtomicFileWriter.write(Data("two\n".utf8), to: target)
    #expect(try Data(contentsOf: target) == Data("two\n".utf8), "an existing file is replaced")
    #expect(try FileManager.default.contentsOfDirectory(atPath: directory.path) == ["note.md"])
  }

  @Test func readOnlyDirectoryFailsWithoutResidue() throws {
    let directory = try Fixtures.temporaryDirectory("atomic-ro")
    defer {
      try? FileManager.default.setAttributes(
        [.posixPermissions: 0o755], ofItemAtPath: directory.path)
      try? FileManager.default.removeItem(at: directory)
    }
    let target = directory.appendingPathComponent("note.md")
    try AtomicFileWriter.write(Data("keep\n".utf8), to: target)
    try FileManager.default.setAttributes([.posixPermissions: 0o555], ofItemAtPath: directory.path)
    guard !FileManager.default.isWritableFile(atPath: directory.path) else {
      return  // running as root: the permission bits do not bite
    }
    let error = #expect(throws: AtomicFileWriter.Failure.self) {
      try AtomicFileWriter.write(Data("new\n".utf8), to: target)
    }
    #expect(error?.path.hasSuffix("note.md") == true)
    #expect(try Data(contentsOf: target) == Data("keep\n".utf8), "the target is untouched")
    #expect(try FileManager.default.contentsOfDirectory(atPath: directory.path) == ["note.md"])
  }

  @Test func staleTemporariesAreRemovedAndNothingElse() throws {
    let directory = try Fixtures.temporaryDirectory("atomic-stale")
    defer { try? FileManager.default.removeItem(at: directory) }
    try Data().write(to: directory.appendingPathComponent(".steno-tmp-note.md-deadbeef"))
    try Data().write(to: directory.appendingPathComponent("notes.md"))
    try Data().write(to: directory.appendingPathComponent(".obsidian-thing"))
    AtomicFileWriter.removeStaleTemporaries(in: directory)
    #expect(
      try FileManager.default.contentsOfDirectory(atPath: directory.path).sorted() == [
        ".obsidian-thing", "notes.md",
      ])
  }

  @Test func temporaryNamesSitBesideTheTargetWithEightHexDigits() {
    let target = URL(fileURLWithPath: "/vault/Meetings/x/note.md")
    let temporary = AtomicFileWriter.temporaryURL(for: target)
    #expect(temporary.deletingLastPathComponent() == target.deletingLastPathComponent())
    #expect(temporary.lastPathComponent.hasPrefix(".steno-tmp-note.md-"))
    let hex = temporary.lastPathComponent.dropFirst(".steno-tmp-note.md-".count)
    #expect(hex.count == 8)
    #expect(hex.allSatisfy { $0.isHexDigit })
  }
}
