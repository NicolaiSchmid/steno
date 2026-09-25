import Foundation
import Testing

@testable import StenoCore

@Suite struct StenoPathsTests {
  @Test func databaseLivesInTheSupportDirectory() {
    let support = URL(fileURLWithPath: "/tmp/steno-support", isDirectory: true)
    let paths = StenoPaths(supportDirectory: support)
    #expect(paths.supportDirectory == support)
    #expect(paths.databaseURL.path == "/tmp/steno-support/steno.sqlite")
  }

  @Test func anAbsoluteHomeInTheEnvironmentWins() throws {
    let home = try Fixtures.temporaryDirectory("home")
    defer { try? FileManager.default.removeItem(at: home) }
    let environment = ["HOME": home.path]
    #expect(StenoPaths.homeDirectory(environment: environment).path == home.path)
    let support = StenoPaths.supportDirectory(environment: environment)
    #expect(support.path == home.appendingPathComponent("Library/Application Support/Steno").path)
    #expect(!FileManager.default.fileExists(atPath: support.path))

    let paths = try StenoPaths.default(environment: environment)
    #expect(paths.supportDirectory.path == support.path)
    #expect(paths.databaseURL.path == support.appendingPathComponent("steno.sqlite").path)
    var isDirectory: ObjCBool = false
    #expect(FileManager.default.fileExists(atPath: support.path, isDirectory: &isDirectory))
    #expect(isDirectory.boolValue)
    #expect(!FileManager.default.fileExists(atPath: paths.databaseURL.path), "no database yet")
  }

  @Test func aRelativeOrMissingHomeFallsBackToFoundation() {
    let fallback = FileManager.default.homeDirectoryForCurrentUser
    #expect(StenoPaths.homeDirectory(environment: [:]) == fallback)
    #expect(StenoPaths.homeDirectory(environment: ["HOME": "relative/home"]) == fallback)
    #expect(StenoPaths.homeDirectory(environment: ["HOME": ""]) == fallback)
    #expect(
      StenoPaths.supportDirectory(environment: [:]).path
        == fallback.appendingPathComponent("Library/Application Support/Steno").path)
  }
}
