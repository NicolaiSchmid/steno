import Foundation
import StenoCore
import Testing

/// Runs the built `steno` binary with `HOME` pointing at a temporary
/// directory, so the real home never gains a `Steno/` folder.
@Suite(.serialized) struct CLITests {
  private final class Marker {}

  /// The products directory that holds `steno`: next to the test bundle on
  /// macOS, next to the test executable on Linux. `STENO_BINARY` overrides.
  static var binary: URL {
    if let override = ProcessInfo.processInfo.environment["STENO_BINARY"] {
      return URL(fileURLWithPath: override)
    }
    #if os(macOS)
      return Bundle(for: Marker.self).bundleURL.deletingLastPathComponent()
        .appendingPathComponent("steno")
    #else
      return Bundle.main.bundleURL.appendingPathComponent("steno")
    #endif
  }

  struct Result {
    var status: Int32
    var stdout: String
    var stderr: String
  }

  static func run(_ arguments: [String], home: URL) throws -> Result {
    let process = Foundation.Process()
    process.executableURL = binary
    process.arguments = arguments
    var environment = ProcessInfo.processInfo.environment
    environment["HOME"] = home.path
    environment.removeValue(forKey: "STENO_UPDATE_SNAPSHOTS")
    process.environment = environment
    process.currentDirectoryURL = home
    let out = Pipe()
    let err = Pipe()
    process.standardOutput = out
    process.standardError = err
    try process.run()
    let stdout = out.fileHandleForReading.readDataToEndOfFile()
    let stderr = err.fileHandleForReading.readDataToEndOfFile()
    process.waitUntilExit()
    return Result(
      status: process.terminationStatus,
      stdout: String(decoding: stdout, as: UTF8.self),
      stderr: String(decoding: stderr, as: UTF8.self))
  }

  static var realStenoFolder: URL {
    FileManager.default.homeDirectoryForCurrentUser
      .appendingPathComponent("Library/Application Support/Steno", isDirectory: true)
  }

  @Test func migrateGenerateProcessAndExport() async throws {
    let home = try Fixtures.temporaryDirectory("steno-home")
    defer { try? FileManager.default.removeItem(at: home) }
    let hadRealFolder = FileManager.default.fileExists(atPath: Self.realStenoFolder.path)
    let db = home.appendingPathComponent("db/steno.sqlite").path

    let migrate = try Self.run(["dev", "db", "migrate", "--db", db], home: home)
    #expect(migrate.status == 0, "\(migrate.stderr)")
    #expect(migrate.stdout.contains("v1"))
    #expect(FileManager.default.fileExists(atPath: db))

    let fixtures = home.appendingPathComponent("fixtures", isDirectory: true)
    let generate = try Self.run(
      ["dev", "fixtures", "generate", "--out", fixtures.path], home: home)
    #expect(generate.status == 0, "\(generate.stderr)")
    #expect(generate.stdout.contains("audio/sweep-3s.wav"))
    #expect(
      FileManager.default.fileExists(
        atPath: fixtures.appendingPathComponent("MANIFEST.sha256").path))
    let generated = try Data(contentsOf: fixtures.appendingPathComponent("audio/sweep-3s.wav"))
    #expect(generated == (try Fixtures.data("audio/sweep-3s.wav")))

    let audio = home.appendingPathComponent("audio", isDirectory: true)
    let process = try Self.run(
      [
        "process", fixtures.appendingPathComponent("audio/sweep-3s.wav").path,
        "--source", "mac-in-person", "--title", "Sweep", "--db", db, "--audio-folder", audio.path,
      ], home: home)
    #expect(process.status == 0, "\(process.stderr)")
    let meetingID = try #require(
      UUID(uuidString: process.stdout.trimmingCharacters(in: .whitespacesAndNewlines)))
    let layout = RecordingLayout(audioFolder: audio, meetingID: meetingID)
    #expect(FileManager.default.fileExists(atPath: layout.master(.wav16kInt16).path))
    #expect(FileManager.default.fileExists(atPath: layout.mixdown(.wav16kInt16).path))
    #expect(
      try FileManager.default.contentsOfDirectory(atPath: layout.speakersDirectory.path).count == 2)
    let stored = try await SettingsStore(
      writer: MeetingStore.onDisk(at: URL(fileURLWithPath: db)).writer
    )
    .load()
    #expect(
      stored.audioFolder == Settings().audioFolder, "--audio-folder never touches the setting")

    let out = home.appendingPathComponent("out", isDirectory: true)
    let export = try Self.run(
      ["export", meetingID.uuidString, "--out", out.path, "--db", db], home: home)
    #expect(export.status == 0, "\(export.stderr)")
    let json = try Data(contentsOf: out.appendingPathComponent("meeting.json"))
    let decoded = try StenoJSON.decode(MeetingExport.self, from: json)
    #expect(decoded.meeting.id == meetingID)
    #expect(decoded.meeting.state == .ready)
    #expect(decoded.meeting.source == .macInPerson)
    #expect(decoded.meeting.title == "Summary of Sweep")
    #expect(decoded.segments.count == 3)
    #expect(decoded.audio?.mixdownURL != nil)

    let reindex = try Self.run(["dev", "db", "reindex", "--db", db], home: home)
    #expect(reindex.status == 0, "\(reindex.stderr)")

    let call = try Self.run(
      [
        "process", fixtures.appendingPathComponent("audio/conversation-mic-6s.wav").path,
        "--system-lane", fixtures.appendingPathComponent("audio/conversation-system-6s.wav").path,
        "--source", "mac-call", "--template", "daily-standup", "--db", db, "--audio-folder",
        audio.path,
      ], home: home)
    #expect(call.status == 0, "\(call.stderr)")
    let callID = try #require(
      UUID(uuidString: call.stdout.trimmingCharacters(in: .whitespacesAndNewlines)))
    let callExport = try Self.run(
      ["export", callID.uuidString, "--out", out.appendingPathComponent("call").path, "--db", db],
      home: home)
    #expect(callExport.status == 0, "\(callExport.stderr)")
    let callDecoded = try StenoJSON.decode(
      MeetingExport.self,
      from: try Data(contentsOf: out.appendingPathComponent("call/meeting.json")))
    #expect(callDecoded.speakers.map(\.clusterLabel) == ["Me", "Speaker 1", "Speaker 2"])
    #expect(callDecoded.meeting.summary?.templateID == "daily-standup")

    #expect(FileManager.default.fileExists(atPath: Self.realStenoFolder.path) == hadRealFolder)
  }

  /// `--engine` and the model commands are parsed and answered without a
  /// download: an unknown engine is a usage error naming the known ones, the
  /// model store lists every asset as absent under a fresh directory, and
  /// removing an absent asset is a no-op. `dev models download` and
  /// `process --engine <real id>` are never run here; they need the models.
  @Test func speechOptionsParseWithoutTouchingModels() throws {
    let home = try Fixtures.temporaryDirectory("steno-home")
    defer { try? FileManager.default.removeItem(at: home) }
    let hadRealFolder = FileManager.default.fileExists(atPath: Self.realStenoFolder.path)
    let db = home.appendingPathComponent("steno.sqlite").path
    let models = home.appendingPathComponent("models", isDirectory: true).path

    let badEngine = try Self.run(
      ["process", Fixtures.url("audio/sweep-3s.wav").path, "--engine", "parakeet-v9", "--db", db],
      home: home)
    #expect(badEngine.status == 1)
    #expect(badEngine.stderr.contains("parakeet-v9"))
    #expect(badEngine.stderr.contains("parakeet-v3"), "the known ids are listed")
    #expect(badEngine.stderr.contains("whisperkit-large-v3-turbo"))

    let help = try Self.run(["process", "--help"], home: home)
    #expect(help.status == 0)
    #expect(help.stdout.contains("--engine <engine>"))
    for id in ["parakeet-v3", "parakeet-ultra", "parakeet-de", "whisperkit-large-v3-turbo"] {
      #expect(help.stdout.contains(id), "\(id)")
    }

    let badBakeoff = try Self.run(
      ["dev", "bakeoff", home.path, "--engines", "nope", "--models-dir", models], home: home)
    #expect(badBakeoff.status == 1)
    #expect(badBakeoff.stderr.contains("nope") && badBakeoff.stderr.contains("parakeet-v3"))

    let list = try Self.run(["dev", "models", "list", "--models-dir", models], home: home)
    #expect(list.status == 0, "\(list.stderr)")
    #expect(list.stdout.contains("models: \(models)"))
    for asset in [
      "parakeetV3", "parakeetUltra", "parakeetDE", "whisperLargeV3Turbo", "offlineDiarizer",
    ] {
      #expect(list.stdout.contains(asset), "\(asset)")
    }
    #expect(
      list.stdout.components(separatedBy: "not installed (~").count == 6, "five absent assets")
    #expect(
      list.stdout.components(separatedBy: "installed (").count == 6,
      "every installed marker is a 'not installed' one")

    let remove = try Self.run(
      ["dev", "models", "remove", "offlineDiarizer", "--models-dir", models], home: home)
    #expect(remove.status == 0, "\(remove.stderr)")
    let badAsset = try Self.run(
      ["dev", "models", "remove", "nope", "--models-dir", models], home: home)
    #expect(badAsset.status == 1)
    #expect(badAsset.stderr.contains("offlineDiarizer"), "the known assets are listed")

    let leftovers = (try? FileManager.default.contentsOfDirectory(atPath: models)) ?? []
    #expect(leftovers.isEmpty, "nothing was downloaded: \(leftovers)")
    #expect(FileManager.default.fileExists(atPath: Self.realStenoFolder.path) == hadRealFolder)
  @Test func deliverWritesTheVaultLayoutFromFlagsOrStoredSettings() async throws {
    let home = try Fixtures.temporaryDirectory("steno-home")
    defer { try? FileManager.default.removeItem(at: home) }
    let db = home.appendingPathComponent("db/steno.sqlite").path
    let fixtures = home.appendingPathComponent("fixtures", isDirectory: true)
    #expect(try Self.run(["dev", "db", "migrate", "--db", db], home: home).status == 0)
    #expect(
      try Self.run(["dev", "fixtures", "generate", "--out", fixtures.path], home: home).status == 0)
    let audio = home.appendingPathComponent("audio", isDirectory: true)
    let process = try Self.run(
      [
        "process", fixtures.appendingPathComponent("audio/sweep-3s.wav").path,
        "--source", "mac-in-person", "--title", "Sweep", "--db", db, "--audio-folder", audio.path,
      ], home: home)
    #expect(process.status == 0, "\(process.stderr)")
    let meetingID = process.stdout.trimmingCharacters(in: .whitespacesAndNewlines)
    #expect(
      try await SettingsStore(writer: MeetingStore.onDisk(at: URL(fileURLWithPath: db)).writer)
        .load().obsidian == nil, "process delivered nowhere: no vault is configured")

    // --vault builds the destination for this run only.
    let vault = home.appendingPathComponent("vault", isDirectory: true)
    try FileManager.default.createDirectory(at: vault, withIntermediateDirectories: true)
    let deliver = try Self.run(
      [
        "deliver", meetingID, "--vault", vault.path, "--people-folder", "People",
        "--include-audio", "--task-tag", "task", "--db", db,
      ], home: home)
    #expect(deliver.status == 0, "\(deliver.stderr)")
    // A --vault run has its own destination id, keyed by the vault, so it
    // never replaces the stored destination's row and receipt.
    let adHoc = "obsidian-folder@\(vault.path)"
    #expect(deliver.stdout.hasPrefix("\(adHoc)\tdelivered\t\(vault.path)/Meetings/"))
    let meetings = vault.appendingPathComponent("Meetings", isDirectory: true)
    let folders = try FileManager.default.contentsOfDirectory(atPath: meetings.path)
    #expect(folders.count == 1)
    let slug = try #require(folders.first)
    #expect(slug.hasSuffix("-summary-of-sweep"))
    let files = try FileManager.default.contentsOfDirectory(
      atPath: meetings.appendingPathComponent(slug).path
    ).sorted()
    #expect(
      files == [
        "\(slug) - Tasks.md", "\(slug) - Transcript.md", "\(slug).md", "audio.wav", "meeting.json",
        "transcript.vtt",
      ], "six files: the WAV decoder's mixdown is copied as audio.wav")
    let json = try Data(
      contentsOf: meetings.appendingPathComponent(slug).appendingPathComponent("meeting.json"))
    #expect(try StenoJSON.decode(MeetingExport.self, from: json).meeting.id.uuidString == meetingID)
    let store = try MeetingStore.onDisk(at: URL(fileURLWithPath: db))
    let rows = try await store.deliveries(meetingID: UUID(uuidString: meetingID)!)
    #expect(rows.map(\.destinationID) == [adHoc])
    #expect(rows.first?.status == .delivered)
    #expect(rows.first?.receipt?.files.count == 6)
    let adHocReceipt = rows.first?.receipt
    #expect(
      try await SettingsStore(writer: store.writer).load().obsidian == nil,
      "--vault never touches the stored settings")

    // Without --vault the stored settings decide.
    let unconfigured = try Self.run(["deliver", meetingID, "--db", db], home: home)
    #expect(unconfigured.status == 2)
    #expect(unconfigured.stderr.contains("No destination configured"))
    let second = home.appendingPathComponent("vault2", isDirectory: true)
    try FileManager.default.createDirectory(at: second, withIntermediateDirectories: true)
    var settings = try await SettingsStore(writer: store.writer).load()
    settings.obsidian = ObsidianSettings(vaultPath: second.path)
    try await SettingsStore(writer: store.writer).save(settings)
    let stored = try Self.run(["deliver", meetingID, "--db", db], home: home)
    #expect(stored.status == 0, "\(stored.stderr)")
    #expect(
      try FileManager.default.contentsOfDirectory(
        atPath: second.appendingPathComponent("Meetings/\(slug)").path
      ).count == 5, "no audio, no people: five files")
    #expect(
      stored.stdout.split(separator: "\n").map { $0.split(separator: "\t").first ?? "" } == [
        "obsidian-folder"
      ], "only this run's destination is printed")
    let bothRows = try await store.deliveries(meetingID: UUID(uuidString: meetingID)!)
    #expect(bothRows.map(\.destinationID).sorted() == ["obsidian-folder", adHoc].sorted())
    #expect(
      bothRows.first { $0.destinationID == adHoc }?.receipt == adHocReceipt,
      "the stored-settings run leaves the --vault row alone")

    let flagsWithoutVault = try Self.run(
      ["deliver", meetingID, "--include-audio", "--db", db], home: home)
    #expect(flagsWithoutVault.status == 1)
    #expect(try Self.run(["deliver", "nope", "--db", db], home: home).status == 1)
    let missingVault = try Self.run(
      ["deliver", meetingID, "--vault", home.appendingPathComponent("absent").path, "--db", db],
      home: home)
    #expect(missingVault.status == 2)
    #expect(missingVault.stderr.contains("does not exist"))

    let unknown = try Self.run(
      ["deliver", UUID().uuidString, "--vault", vault.path, "--db", db], home: home)
    #expect(unknown.status == 2)
    #expect(unknown.stderr.contains("not found"))

    // A failed destination is a printed row and exit 2; the other files are
    // still written. The mixdown is removed so --include-audio has nothing
    // to copy.
    let mixdown = RecordingLayout(audioFolder: audio, meetingID: UUID(uuidString: meetingID)!)
      .mixdown(.wav16kInt16)
    try FileManager.default.removeItem(at: mixdown)
    let third = home.appendingPathComponent("vault3", isDirectory: true)
    try FileManager.default.createDirectory(at: third, withIntermediateDirectories: true)
    let partial = try Self.run(
      ["deliver", meetingID, "--vault", third.path, "--include-audio", "--db", db], home: home)
    #expect(partial.status == 2)
    #expect(partial.stdout.hasPrefix("obsidian-folder@\(third.path)\tfailed\t"))
    #expect(partial.stdout.contains("no audio mixdown"))
    #expect(partial.stderr.contains("delivery failed"))
    #expect(
      try FileManager.default.contentsOfDirectory(
        atPath: third.appendingPathComponent("Meetings/\(slug)").path
      ).count == 5, "every other file was written before the failure")
    let allRows = try await store.deliveries(meetingID: UUID(uuidString: meetingID)!)
    let failedRow = allRows.first { $0.destinationID == "obsidian-folder@\(third.path)" }
    #expect(failedRow?.status.kind == .failed)
    #expect(failedRow?.receipt == nil, "a failed first delivery has no receipt")
    let storedRow = allRows.first { $0.destinationID == "obsidian-folder" }
    #expect(storedRow?.status == .delivered)
    #expect(storedRow?.receipt?.root == second.path, "the stored destination's receipt is kept")
  }

  @Test func defaultDatabaseFollowsHome() throws {
    let home = try Fixtures.temporaryDirectory("steno-home")
    defer { try? FileManager.default.removeItem(at: home) }
    let hadRealFolder = FileManager.default.fileExists(atPath: Self.realStenoFolder.path)
    let migrate = try Self.run(["dev", "db", "migrate"], home: home)
    #expect(migrate.status == 0, "\(migrate.stderr)")
    #expect(
      FileManager.default.fileExists(
        atPath: home.appendingPathComponent("Library/Application Support/Steno/steno.sqlite").path))
    #expect(FileManager.default.fileExists(atPath: Self.realStenoFolder.path) == hadRealFolder)
  }

  @Test func exitCodesDistinguishUsageFromRuntimeFailures() throws {
    let home = try Fixtures.temporaryDirectory("steno-home")
    defer { try? FileManager.default.removeItem(at: home) }
    let db = home.appendingPathComponent("steno.sqlite").path

    let usage = try Self.run(["process"], home: home)
    #expect(usage.status == 1)
    #expect(usage.stderr.contains("Missing expected argument"))

    let badTemplate = try Self.run(
      ["process", Fixtures.url("audio/sweep-3s.wav").path, "--template", "nope", "--db", db],
      home: home)
    #expect(badTemplate.status == 1)
    #expect(badTemplate.stderr.contains("Unknown template nope"))

    let missingLane = try Self.run(
      ["process", Fixtures.url("audio/sweep-3s.wav").path, "--source", "mac-call", "--db", db],
      home: home)
    #expect(missingLane.status == 1)

    let notFound = try Self.run(
      ["export", UUID().uuidString, "--out", home.path, "--db", db], home: home)
    #expect(notFound.status == 2)
    #expect(notFound.stderr.contains("not found"))

    let notAUUID = try Self.run(["export", "nope", "--db", db], home: home)
    #expect(notAUUID.status == 1)

    let garbage = home.appendingPathComponent("garbage.wav")
    try Data(repeating: 0x41, count: 64).write(to: garbage)
    let malformed = try Self.run(
      [
        "process", garbage.path, "--db", db, "--audio-folder",
        home.appendingPathComponent("audio").path,
      ], home: home)
    #expect(malformed.status == 2)
    #expect(malformed.stderr.contains("RIFF"))

    let unwritable = try Self.run(
      ["dev", "db", "migrate", "--db", garbage.appendingPathComponent("steno.sqlite").path],
      home: home)
    #expect(unwritable.status == 2)

    let version = try Self.run(["--version"], home: home)
    #expect(version.status == 0)
    #expect(version.stdout.trimmingCharacters(in: .whitespacesAndNewlines) == StenoCore.version)

    let help = try Self.run(["dev", "--help"], home: home)
    #expect(help.status == 0)
    #expect(help.stdout.contains("fixtures"))
  }
}
