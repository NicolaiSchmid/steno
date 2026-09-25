import Foundation
import StenoCore
import Testing

@testable import StenoAdapters

/// The destination against a real temp vault: first delivery, validation,
/// re-export, collision, preservation. Every test gets a fresh directory.
@Suite struct ObsidianDestinationIntegrationTests {
  struct Vault {
    let directory: URL
    let root: URL

    init() throws {
      directory = try Fixtures.temporaryDirectory("vault")
      root = directory.appendingPathComponent("vault", isDirectory: true)
      try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    }

    func settings(includeAudio: Bool = true, peopleFolder: String? = "People") -> ObsidianSettings {
      ObsidianSettings(
        vaultPath: root.path, peopleFolder: peopleFolder, includeAudio: includeAudio,
        taskTag: "task")
    }

    func destination(includeAudio: Bool = true, peopleFolder: String? = "People")
      -> ObsidianFolderDestination
    {
      ObsidianFolderDestination(
        settings: settings(includeAudio: includeAudio, peopleFolder: peopleFolder),
        timeZone: FixtureMeeting.berlin)
    }

    func url(_ relative: String) -> URL { root.appendingPathComponent(relative) }
    func read(_ relative: String) throws -> Data { try Data(contentsOf: url(relative)) }
    func text(_ relative: String) throws -> String {
      String(decoding: try read(relative), as: UTF8.self)
    }
    func list(_ relative: String) throws -> [String] {
      try FileManager.default.contentsOfDirectory(atPath: url(relative).path).sorted()
    }
    func cleanUp() { try? FileManager.default.removeItem(at: directory) }
  }

  static let slug = FixtureMeeting.folderSlug
  static let folder = FixtureMeeting.folder
  static let meetingFiles = meetingFiles(slug: slug)

  /// The six files of a meeting folder, in directory-listing order.
  static func meetingFiles(slug: String) -> [String] {
    [
      "\(slug) - Tasks.md", "\(slug) - Transcript.md", "\(slug).md", "audio.m4a", "meeting.json",
      "transcript.vtt",
    ]
  }

  @Test func firstDeliveryWritesTheSixFileLayoutAndTwoPersonPages() async throws {
    let vault = try Vault()
    defer { vault.cleanUp() }
    let export = try FixtureMeeting.export(audioIn: vault.directory)
    let destination = vault.destination()
    try await destination.validate()

    let receipt = try await destination.deliver(export, previous: nil)

    #expect(try vault.list("Meetings") == [Self.slug])
    #expect(try vault.list(Self.folder) == Self.meetingFiles)
    #expect(try vault.list("People") == ["Anna Müller.md", "Nicolai Schmid.md"])
    let renderer = ArtifactRenderer()
    let options = FixtureMeeting.wikilinkTag
    #expect(
      try vault.text("\(Self.folder)/\(Self.slug).md")
        == renderer.renderFolderNote(export, options: options))
    try Snapshot.assert(
      try vault.read("\(Self.folder)/\(Self.slug).md"),
      matches: "snapshots/obsidian/folder-note-wikilink-berlin.md")
    try Snapshot.assert(
      try vault.read("\(Self.folder)/\(Self.slug) - Transcript.md"),
      matches: "snapshots/obsidian/transcript-wikilink.md")
    try Snapshot.assert(
      try vault.read("\(Self.folder)/\(Self.slug) - Tasks.md"),
      matches: "snapshots/obsidian/tasks-wikilink-tag.md")
    try Snapshot.assert(
      try vault.read("\(Self.folder)/transcript.vtt"), matches: "snapshots/obsidian/transcript.vtt")
    #expect(
      try vault.read("\(Self.folder)/meeting.json") == renderer.renderJSON(export),
      "meeting.json carries this run's mixdown path, so it is compared with the renderer")
    #expect(try vault.read("\(Self.folder)/audio.m4a") == Data((0..<100).map { UInt8($0) }))
    try Snapshot.assert(
      try vault.read("People/Anna Müller.md"), matches: "snapshots/obsidian/person-page-new.md")

    #expect(receipt.root == vault.root.path)
    #expect(receipt.folder == Self.folder)
    #expect(receipt.rendererVersion == ArtifactRenderer.version)
    #expect(
      receipt.files.map(\.relativePath) == Self.meetingFiles.map { "\(Self.folder)/\($0)" } + [
        "People/Anna Müller.md", "People/Nicolai Schmid.md",
      ])
    #expect(receipt.files.filter { $0.ownership == .owned }.count == 6)
    #expect(receipt.files.filter { $0.ownership == .managedBlock }.count == 2)
    for file in receipt.files {
      #expect(
        file.sha256 == ContentHash.sha256(try vault.read(file.relativePath)),
        "\(file.relativePath) hash")
    }
    #expect(
      try vault.list(Self.folder).allSatisfy { !$0.hasPrefix(ObsidianLayout.temporaryPrefix) })
  }

  @Test func validateRejectsMissingUnwritableAndBadPeopleFolder() async throws {
    let vault = try Vault()
    defer { vault.cleanUp() }
    let missing = ObsidianFolderDestination(
      settings: ObsidianSettings(vaultPath: vault.directory.appendingPathComponent("nope").path))
    await #expect(throws: ObsidianError.vaultMissing(missing.settings.vaultPath)) {
      try await missing.validate()
    }
    let file = vault.directory.appendingPathComponent("file")
    try Data().write(to: file)
    await #expect(throws: ObsidianError.vaultMissing(file.path)) {
      try await ObsidianFolderDestination(settings: ObsidianSettings(vaultPath: file.path))
        .validate()
    }

    for bad in ["/People", "../People", "People/../..", "", "a//b", "a\\b"] {
      await #expect(throws: ObsidianError.peopleFolderInvalid(bad), "\(bad)") {
        try await vault.destination(peopleFolder: bad).validate()
      }
    }
    try await vault.destination(peopleFolder: "Notes/People").validate()
    try await vault.destination(peopleFolder: nil).validate()
    #expect(try vault.list("").isEmpty, "the probe leaves nothing behind")

    let locked = vault.directory.appendingPathComponent("locked", isDirectory: true)
    try FileManager.default.createDirectory(at: locked, withIntermediateDirectories: true)
    try FileManager.default.setAttributes([.posixPermissions: 0o555], ofItemAtPath: locked.path)
    defer {
      try? FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: locked.path)
    }
    if !FileManager.default.isWritableFile(atPath: locked.path) {
      await #expect(throws: ObsidianError.vaultNotWritable(locked.path)) {
        try await ObsidianFolderDestination(settings: ObsidianSettings(vaultPath: locked.path))
          .validate()
      }
      let export = FixtureMeeting.export()
      let error = await #expect(throws: ObsidianError.self) {
        try await ObsidianFolderDestination(settings: ObsidianSettings(vaultPath: locked.path))
          .deliver(export, previous: nil)
      }
      if case .writeFailed(let path, _)? = error {
        #expect(path.hasPrefix(locked.path))
      } else {
        Issue.record("expected writeFailed, got \(String(describing: error))")
      }
    }
    #expect(
      ObsidianError.audioUnavailable.description.contains("no audio mixdown"),
      "messages are shown verbatim by the app")
  }
}
