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
    #expect(deliver.stdout.hasPrefix("obsidian-folder\tdelivered\t\(vault.path)/Meetings/"))
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
    #expect(rows.map(\.destinationID) == ["obsidian-folder"])
    #expect(rows.first?.status == .delivered)
    #expect(rows.first?.receipt?.files.count == 6)
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

    let flagsWithoutVault = try Self.run(
      ["deliver", meetingID, "--include-audio", "--db", db], home: home)
    #expect(flagsWithoutVault.status == 1)
    #expect(try Self.run(["deliver", "nope", "--db", db], home: home).status == 1)
    let missingVault = try Self.run(
      ["deliver", meetingID, "--vault", home.appendingPathComponent("absent").path, "--db", db],
      home: home)
    #expect(missingVault.status == 2)
    #expect(missingVault.stderr.contains("does not exist"))
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
