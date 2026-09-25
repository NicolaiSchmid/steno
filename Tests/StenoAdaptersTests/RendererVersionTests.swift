import Foundation
import StenoCore
import Testing

@testable import StenoAdapters

/// `snapshots/obsidian/VERSION` holds `ArtifactRenderer.version` and a
/// SHA-256 over every golden in that folder, so a golden that changes
/// without a version bump fails here. `STENO_UPDATE_SNAPSHOTS=1` rewrites it
/// (run the goldens first, then this).
@Suite struct RendererVersionTests {
  static let folder = "snapshots/obsidian"

  static func manifest() throws -> String {
    let directory = Fixtures.url(folder)
    let names = try FileManager.default.contentsOfDirectory(atPath: directory.path)
      .filter { $0 != "VERSION" && !$0.hasSuffix(".actual") && !$0.hasPrefix(".") }
      .sorted()
    return names.map { name in
      let data = (try? Data(contentsOf: directory.appendingPathComponent(name))) ?? Data()
      return "\(ContentHash.sha256Hex(data))  \(name)"
    }
    .joined(separator: "\n") + "\n"
  }

  @Test func goldensAreCoveredByTheRecordedVersion() throws {
    let manifest = try Self.manifest()
    #expect(manifest.split(separator: "\n").count >= 14, "every renderer variant has a golden")
    let expected = "\(ArtifactRenderer.version) \(ContentHash.sha256Hex(Data(manifest.utf8)))\n"
    try Snapshot.assert(expected, matches: "\(Self.folder)/VERSION")
  }

  @Test func theReceiptCarriesTheRendererVersion() async throws {
    let directory = try Fixtures.temporaryDirectory("version")
    defer { try? FileManager.default.removeItem(at: directory) }
    let destination = ObsidianFolderDestination(
      settings: ObsidianSettings(vaultPath: directory.path), timeZone: FixtureMeeting.berlin)
    let receipt = try await destination.deliver(FixtureMeeting.export(), previous: nil)
    #expect(receipt.rendererVersion == ArtifactRenderer.version)
    let recorded = try String(contentsOf: Fixtures.url("\(Self.folder)/VERSION"), encoding: .utf8)
    #expect(recorded.hasPrefix("\(ArtifactRenderer.version) "))
  }
}
