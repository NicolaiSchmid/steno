import Foundation
import Testing

@testable import StenoCore

@Suite struct SnapshotTests {
  @Test func missingGoldenThrowsAndUpdateCreatesIt() throws {
    let root = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: root) }
    #expect(throws: Snapshot.Missing.self) {
      try Snapshot.assert(Data("a\n".utf8), matches: "x/golden.txt", root: root, environment: [:])
    }
    try Snapshot.assert(
      Data("a\n".utf8), matches: "x/golden.txt", root: root,
      environment: [Snapshot.updateEnvironmentKey: "1"])
    #expect(
      try String(contentsOf: root.appendingPathComponent("x/golden.txt"), encoding: .utf8) == "a\n")
    try Snapshot.assert(Data("a\n".utf8), matches: "x/golden.txt", root: root, environment: [:])
  }

  @Test func mismatchThrowsAUnifiedDiffAndWritesTheActualBytes() throws {
    let root = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: root) }
    try Data("one\ntwo\nthree\n".utf8).write(to: root.appendingPathComponent("g.txt"))
    let error = #expect(throws: Snapshot.Mismatch.self) {
      try Snapshot.assert(
        Data("one\n2\nthree\n".utf8), matches: "g.txt", root: root, environment: [:])
    }
    let diff = try #require(error?.diff)
    #expect(diff.contains("--- expected"))
    #expect(diff.contains("+++ actual"))
    #expect(diff.contains("-two"))
    #expect(diff.contains("+2"))
    #expect(diff.contains(" one"))
    #expect(
      try String(contentsOf: root.appendingPathComponent("g.txt.actual"), encoding: .utf8)
        == "one\n2\nthree\n")
    #expect(
      try String(contentsOf: root.appendingPathComponent("g.txt"), encoding: .utf8)
        == "one\ntwo\nthree\n")
  }

  @Test func unifiedDiffHandlesInsertionsAndDeletions() {
    let diff = Snapshot.unifiedDiff(expected: "a\nb\nc", actual: "a\nc\nd")
    #expect(diff == "--- expected\n+++ actual\n a\n-b\n c\n+d")
  }
}
