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
      try vault.list(Self.folder).allSatisfy { !$0.hasPrefix(AtomicFileWriter.temporaryPrefix) })
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

  @Test func reexportKeepsTheFolderUserFilesAndEditsAndUpdatesTheTitle() async throws {
    let vault = try Vault()
    defer { vault.cleanUp() }
    let export = try FixtureMeeting.export(audioIn: vault.directory)
    let first = try await vault.destination().deliver(export, previous: nil)

    // The user adds a note, edits Anna's page around the block and Steno
    // renames the meeting; audio is opted out.
    try Data("my notes\n".utf8).write(to: vault.url("\(Self.folder)/notes.md"))
    let anna = "People/Anna Müller.md"
    let edited = "Above the block.\n\n" + (try vault.text(anna)) + "\nBelow the block.\n"
    try Data(edited.utf8).write(to: vault.url(anna))
    var renamed = export
    renamed.meeting.title = "Neuer Titel nach dem Re-Run"

    let second = try await vault.destination(includeAudio: false).deliver(renamed, previous: first)

    #expect(second.folder == Self.folder, "the folder is pinned at first delivery")
    #expect(try vault.list("Meetings") == [Self.slug])
    #expect(try vault.list(Self.folder) == (Self.meetingFiles + ["notes.md"]).sorted())
    #expect(try vault.text("\(Self.folder)/notes.md") == "my notes\n")
    #expect(
      try vault.read("\(Self.folder)/audio.m4a") == Data((0..<100).map { UInt8($0) }),
      "an opted-out audio copy stays")
    let note = try vault.text("\(Self.folder)/\(Self.slug).md")
    #expect(note.hasPrefix("---\ntitle: \"Neuer Titel nach dem Re-Run\"\n"))
    #expect(note.contains("# Neuer Titel nach dem Re-Run\n"))
    #expect(
      note.contains("[[\(Self.slug) - Transcript|Transcript]]"),
      "note links keep the pinned slug")
    let page = try vault.text(anna)
    #expect(page.hasPrefix("Above the block.\n\n---\n"))
    #expect(page.hasSuffix("<!-- steno:meetings:end -->\n\nBelow the block.\n"))
    #expect(page.contains("[[\(Self.slug)|Neuer Titel nach dem Re-Run]] %%steno:"))
    #expect(!page.contains("Roadmap für Q4]]"), "the old line for this meeting is replaced")
    #expect(
      second.files.map(\.relativePath) == first.files.map(\.relativePath),
      "audio stays in the receipt")
    let audio = second.files.first { $0.relativePath.hasSuffix("audio.m4a") }
    #expect(audio == first.files.first { $0.relativePath.hasSuffix("audio.m4a") })
    #expect(second.rendererVersion == ArtifactRenderer.version)
  }

  @Test func unchangedMeetingReexportsByteIdentically() async throws {
    let vault = try Vault()
    defer { vault.cleanUp() }
    let export = try FixtureMeeting.export(audioIn: vault.directory)
    let destination = vault.destination()
    let first = try await destination.deliver(export, previous: nil)
    let before = try Dictionary(
      uniqueKeysWithValues: first.files.map { ($0.relativePath, try vault.read($0.relativePath)) })
    let second = try await destination.deliver(export, previous: first)
    #expect(second == first)
    for (path, data) in before {
      #expect(try vault.read(path) == data, "\(path)")
    }
  }

  @Test func collisionsGetASuffixAndACrashedAttemptIsReused() async throws {
    let vault = try Vault()
    defer { vault.cleanUp() }
    let export = try FixtureMeeting.export(audioIn: vault.directory)

    // Somebody else's meeting already owns the folder; an unrelated folder
    // without meeting.json counts as taken too.
    var other = export
    other.meeting.id = SampleData.uuid(99)
    try FileManager.default.createDirectory(
      at: vault.url(Self.folder), withIntermediateDirectories: true)
    try StenoJSON.encode(other).write(to: vault.url("\(Self.folder)/meeting.json"))
    try FileManager.default.createDirectory(
      at: vault.url("\(Self.folder)-2"), withIntermediateDirectories: true)
    try Data("theirs\n".utf8).write(to: vault.url("\(Self.folder)-2/notes.md"))

    let receipt = try await vault.destination().deliver(export, previous: nil)
    #expect(receipt.folder == "\(Self.folder)-3")
    #expect(try vault.list("Meetings") == [Self.slug, "\(Self.slug)-2", "\(Self.slug)-3"])
    #expect(try vault.list("\(Self.folder)-2") == ["notes.md"], "the taken folders are untouched")
    #expect(try vault.list(Self.folder) == ["meeting.json"])
    #expect(
      try vault.list("\(Self.folder)-3").contains("\(Self.slug)-3.md"),
      "notes are named after the suffixed folder")
    let note = try vault.text("\(Self.folder)-3/\(Self.slug)-3.md")
    #expect(note.contains("[[\(Self.slug)-3 - Transcript|Transcript]]"))
    #expect(
      try vault.text("People/Anna Müller.md").contains("[[\(Self.slug)-3|Produktstrategie"))

    // A first delivery for a meeting whose folder already holds its own
    // meeting.json (a crash between write and receipt) reuses the folder.
    let again = try await vault.destination().deliver(export, previous: nil)
    #expect(again.folder == "\(Self.folder)-3")
    #expect(try vault.list("Meetings") == [Self.slug, "\(Self.slug)-2", "\(Self.slug)-3"])
  }

  @Test func filesTheAppNeverWroteAreNotOpenedOnReexport() async throws {
    let vault = try Vault()
    defer { vault.cleanUp() }
    let export = try FixtureMeeting.export(audioIn: vault.directory)
    let first = try await vault.destination(includeAudio: false).deliver(export, previous: nil)
    #expect(!first.files.contains { $0.relativePath.hasSuffix("audio.m4a") })
    // The user drops their own audio.m4a into the folder; audio is then
    // switched on.
    try Data("user audio\n".utf8).write(to: vault.url("\(Self.folder)/audio.m4a"))
    let second = try await vault.destination(includeAudio: true).deliver(export, previous: first)
    #expect(try vault.text("\(Self.folder)/audio.m4a") == "user audio\n")
    #expect(!second.files.contains { $0.relativePath.hasSuffix("audio.m4a") })
  }

  @Test func missingMixdownFailsAfterEveryOtherFileIsWritten() async throws {
    let vault = try Vault()
    defer { vault.cleanUp() }
    var export = FixtureMeeting.export()
    export.audio?.mixdownURL = nil
    await #expect(throws: ObsidianError.audioUnavailable) {
      try await vault.destination().deliver(export, previous: nil)
    }
    #expect(try vault.list(Self.folder) == Self.meetingFiles.filter { $0 != "audio.m4a" })
    #expect(try vault.list("People").count == 2)
  }

  @Test func staleTemporariesAreSweptAndPeopleOffWritesNoPages() async throws {
    let vault = try Vault()
    defer { vault.cleanUp() }
    try FileManager.default.createDirectory(
      at: vault.url(Self.folder), withIntermediateDirectories: true)
    try Data("ours\n".utf8).write(to: vault.url("\(Self.folder)/.steno-tmp-x.md-00000000"))
    let export = try FixtureMeeting.export(audioIn: vault.directory)
    // The folder exists without meeting.json, so the delivery goes to -2 and
    // sweeps only its own folder.
    let receipt = try await vault.destination(peopleFolder: nil).deliver(export, previous: nil)
    #expect(receipt.folder == "\(Self.folder)-2")
    #expect(try vault.list("\(Self.folder)-2") == Self.meetingFiles(slug: "\(Self.slug)-2"))
    #expect(
      try vault.list(Self.folder) == [".steno-tmp-x.md-00000000"],
      "another folder's temp is not ours to sweep")
    #expect(!FileManager.default.fileExists(atPath: vault.url("People").path))
    #expect(receipt.files.count == 6)
    let note = try vault.text("\(Self.folder)-2/\(Self.slug)-2.md")
    #expect(note.contains("  - \"Anna Müller\"\n"), "no people folder, no links")
  }

  @Test func firstDeliveryAppendsTheBlockToAPersonPageTheUserAlreadyWrote() async throws {
    let vault = try Vault()
    defer { vault.cleanUp() }
    let export = try FixtureMeeting.export(audioIn: vault.directory)
    try FileManager.default.createDirectory(
      at: vault.url("People"), withIntermediateDirectories: true)
    let userPage = "---\nrole: \"CEO\"\n---\n# Anna\n\nMet her at the fair."
    try Data(userPage.utf8).write(to: vault.url("People/Anna Müller.md"))
    try Data("Bob's page.\n".utf8).write(to: vault.url("People/Bob.md"))

    let receipt = try await vault.destination().deliver(export, previous: nil)

    let anna = try vault.text("People/Anna Müller.md")
    #expect(
      anna.hasPrefix(userPage + "\n\n<!-- steno:meetings:start -->\n- 2026-09-24 [["),
      "the user's frontmatter and text come first, then the block")
    #expect(anna.hasSuffix("<!-- steno:meetings:end -->\n"))
    #expect(!anna.contains("steno_person_id"), "an existing page never gets Steno's frontmatter")
    #expect(try vault.text("People/Bob.md") == "Bob's page.\n")
    #expect(try vault.list("People") == ["Anna Müller.md", "Bob.md", "Nicolai Schmid.md"])
    #expect(!receipt.files.contains { $0.relativePath == "People/Bob.md" })
    #expect(
      receipt.files.first { $0.relativePath == "People/Anna Müller.md" }?.ownership
        == .managedBlock)
  }

  @Test func reexportRewritesOwnedNotesAndRecreatesADeletedOne() async throws {
    let vault = try Vault()
    defer { vault.cleanUp() }
    let export = try FixtureMeeting.export(audioIn: vault.directory)
    let destination = vault.destination()
    let first = try await destination.deliver(export, previous: nil)
    let note = "\(Self.folder)/\(Self.slug).md"
    let original = try vault.read(note)

    // The folder note is wholly Steno's: an edit there is lost on re-export
    // (the scratchpad in the app is the place for notes). A deleted file
    // Steno wrote comes back.
    let edited = (try vault.text(note)) + "\nMy addition.\n"
    try Data(edited.utf8).write(to: vault.url(note))
    try FileManager.default.removeItem(at: vault.url("\(Self.folder)/transcript.vtt"))

    let second = try await destination.deliver(export, previous: first)

    #expect(try vault.read(note) == original, "an owned note is rewritten from the model")
    #expect(try vault.list(Self.folder) == Self.meetingFiles, "transcript.vtt is back")
    try Snapshot.assert(
      try vault.read("\(Self.folder)/transcript.vtt"), matches: "snapshots/obsidian/transcript.vtt")
    #expect(second == first)
  }

  @Test func peopleFolderOffKeepsThePagesOnDiskAndInTheReceipt() async throws {
    let vault = try Vault()
    defer { vault.cleanUp() }
    let export = try FixtureMeeting.export(audioIn: vault.directory)
    let first = try await vault.destination().deliver(export, previous: nil)
    let anna = try vault.read("People/Anna Müller.md")

    let second = try await vault.destination(peopleFolder: nil).deliver(export, previous: first)

    #expect(try vault.list("People") == ["Anna Müller.md", "Nicolai Schmid.md"])
    #expect(try vault.read("People/Anna Müller.md") == anna, "not touched, not deleted")
    #expect(second.files.map(\.relativePath) == first.files.map(\.relativePath))
    #expect(second.files.filter { $0.ownership == .managedBlock }.count == 2)
    let note = try vault.text("\(Self.folder)/\(Self.slug).md")
    #expect(note.contains("  - \"Anna Müller\"\n"), "the notes stop linking people")
    #expect(!note.contains("[[Anna"))
  }

  @Test func aSecondMeetingOnTheSameDayGetsTheNextSuffixAndSharesThePersonPages() async throws {
    let vault = try Vault()
    defer { vault.cleanUp() }
    let destination = vault.destination(includeAudio: false)
    let first = FixtureMeeting.export()
    var second = first
    second.meeting.id = SampleData.uuid(2)
    second.meeting.startedAt = first.meeting.startedAt.addingTimeInterval(3 * 3600)

    let receiptOne = try await destination.deliver(first, previous: nil)
    let filesOne = try Dictionary(
      uniqueKeysWithValues: receiptOne.files.map {
        ($0.relativePath, try vault.read($0.relativePath))
      })
    let receiptTwo = try await destination.deliver(second, previous: nil)

    #expect(receiptOne.folder == Self.folder)
    #expect(receiptTwo.folder == "\(Self.folder)-2")
    #expect(try vault.list("Meetings") == [Self.slug, "\(Self.slug)-2"])
    #expect(
      try vault.list("\(Self.folder)-2")
        == Self.meetingFiles(slug: "\(Self.slug)-2").filter { $0 != "audio.m4a" })
    for (path, data) in filesOne where !path.hasPrefix("People/") {
      #expect(try vault.read(path) == data, "\(path): the first meeting's files are untouched")
    }
    let anna = try vault.text("People/Anna Müller.md")
    let lines = anna.split(separator: "\n").filter { $0.hasPrefix("- 2026-09-24 ") }.map(
      String.init)
    #expect(lines.count == 2, "one line per meeting")
    #expect(anna.contains("%%steno:00000000-0000-0000-0000-000000000001%%"))
    #expect(anna.contains("%%steno:00000000-0000-0000-0000-000000000002%%"))
    #expect(
      anna.contains("[[\(Self.slug)-2|Produktstrategie"), "the line links the suffixed folder")
    #expect(anna.components(separatedBy: ManagedBlock.start).count == 2, "one block")
    #expect(lines == ManagedBlock.sortedNewestFirst(lines), "the block is in sorted order")

    // Each meeting re-exports to its own pinned folder.
    let again = try await destination.deliver(second, previous: receiptTwo)
    #expect(again == receiptTwo)
    #expect(try vault.list("Meetings") == [Self.slug, "\(Self.slug)-2"])
  }

  @Test func aRenamedPersonGetsANewPageAndTheOldOneStays() async throws {
    let vault = try Vault()
    defer { vault.cleanUp() }
    let export = try FixtureMeeting.export(audioIn: vault.directory)
    let first = try await vault.destination().deliver(export, previous: nil)
    let oldPage = try vault.read("People/Anna Müller.md")

    var renamed = export
    renamed.persons[0].displayName = "Anna Schulz"
    renamed.participants[0].displayName = "Anna Schulz"
    let second = try await vault.destination().deliver(renamed, previous: first)

    #expect(
      try vault.list("People") == ["Anna Müller.md", "Anna Schulz.md", "Nicolai Schmid.md"])
    #expect(try vault.read("People/Anna Müller.md") == oldPage, "never deleted, never rewritten")
    let newPage = try vault.text("People/Anna Schulz.md")
    #expect(newPage.contains("# Anna Schulz\n"))
    #expect(newPage.contains("steno_person_id: \"00000000-0000-0000-0000-00000000000a\""))
    #expect(
      second.files.filter { $0.ownership == .managedBlock }.map(\.relativePath) == [
        "People/Anna Müller.md", "People/Anna Schulz.md", "People/Nicolai Schmid.md",
      ], "the old page stays in the receipt")
    let transcript = try vault.text("\(Self.folder)/\(Self.slug) - Transcript.md")
    #expect(transcript.contains("## [[Anna Schulz]] — 00:00:04"))
    #expect(!transcript.contains("Anna Müller"), "every note uses the current name")
  }

  @Test func aMovedVaultIsWrittenFreshUnderThePinnedFolderAndTheOldOneIsLeftAlone() async throws {
    let vault = try Vault()
    defer { vault.cleanUp() }
    let export = try FixtureMeeting.export(audioIn: vault.directory)
    let first = try await vault.destination().deliver(export, previous: nil)
    let before = try Dictionary(
      uniqueKeysWithValues: first.files.map { ($0.relativePath, try vault.read($0.relativePath)) })

    let moved = vault.directory.appendingPathComponent("moved", isDirectory: true)
    try FileManager.default.createDirectory(at: moved, withIntermediateDirectories: true)
    let destination = ObsidianFolderDestination(
      settings: ObsidianSettings(
        vaultPath: moved.path, peopleFolder: "People", includeAudio: true, taskTag: "task"),
      timeZone: FixtureMeeting.berlin)
    let second = try await destination.deliver(export, previous: first)

    #expect(second.root == moved.path)
    #expect(second.folder == first.folder, "the folder name travels with the receipt")
    #expect(second.files.map(\.relativePath) == first.files.map(\.relativePath))
    for file in second.files {
      #expect(
        try Data(contentsOf: moved.appendingPathComponent(file.relativePath))
          == before[file.relativePath], "\(file.relativePath) in the new vault")
    }
    for (path, data) in before {
      #expect(try vault.read(path) == data, "\(path) in the old vault")
    }
  }

  @Test func aMixdownPathWithoutAFileIsAudioUnavailable() async throws {
    let vault = try Vault()
    defer { vault.cleanUp() }
    var export = FixtureMeeting.export()
    export.audio?.mixdownURL = vault.directory.appendingPathComponent("gone.m4a")
    await #expect(throws: ObsidianError.audioUnavailable) {
      try await vault.destination().deliver(export, previous: nil)
    }
    #expect(try vault.list(Self.folder) == Self.meetingFiles.filter { $0 != "audio.m4a" })
    let noAudio = try await vault.destination(includeAudio: false).deliver(export, previous: nil)
    #expect(noAudio.files.count == 7, "with audio off the missing mixdown is no error")
  }
}
