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
