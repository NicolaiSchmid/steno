import Foundation
import StenoAudio
import StenoCore
import Testing

#if canImport(Glibc)
  import Glibc
#elseif canImport(Darwin)
  import Darwin
#endif

/// `steno record --backend synthetic` through the built binary: the SIGKILL
/// check (a killed recorder leaves a readable master) and the in-person
/// folder shape. Wall time enters only where a process is killed mid-run.
@Suite(.serialized) struct RecordCLITests {
  @Test func sigkillLeavesReadableCAF() throws {
    let home = try Fixtures.temporaryDirectory("steno-home")
    defer { try? FileManager.default.removeItem(at: home) }
    let audio = home.appendingPathComponent("audio", isDirectory: true)
    let meetingID = UUID()

    let process = Foundation.Process()
    process.executableURL = CLITests.binary
    process.arguments = [
      "record", "--backend", "synthetic", "--seconds", "30", "--out", audio.path,
      "--meeting-id", meetingID.uuidString, "--quiet",
    ]
    var environment = ProcessInfo.processInfo.environment
    environment["HOME"] = home.path
    process.environment = environment
    process.standardOutput = Pipe()
    process.standardError = Pipe()
    try process.run()
    Thread.sleep(forTimeInterval: 2)
    kill(process.processIdentifier, SIGKILL)
    process.waitUntilExit()
    #expect(process.terminationStatus != 0)

    let layout = RecordingLayout(audioFolder: audio, meetingID: meetingID)
    let master = try CAFFile.read(layout.master(.caf48kFloat32))
    #expect(master.sampleRate == 48_000)
    #expect(master.channels.count == 2)
    #expect(abs(master.duration - 2) < 1, "\(master.duration) s written before SIGKILL")
    #expect(master.channels[1][master.frameCount - 1] != 0, "the system lane carries the tone")
  }

  @Test func inPersonSyntheticRecordingProducesOneChannelAndTheMixedSidecar() throws {
    let home = try Fixtures.temporaryDirectory("steno-home")
    defer { try? FileManager.default.removeItem(at: home) }
    let audio = home.appendingPathComponent("audio", isDirectory: true)
    let meetingID = UUID()
    let result = try CLITests.run(
      [
        "record", "--mode", "in-person", "--backend", "synthetic", "--seconds", "0.5", "--out",
        audio.path, "--meeting-id", meetingID.uuidString, "--quiet",
      ], home: home)
    #expect(result.status == 0, "\(result.stderr)")
    #expect(result.stdout.contains("meeting: \(meetingID.uuidString)"))
    #expect(result.stdout.contains("dropped frames: none"))
    let layout = RecordingLayout(audioFolder: audio, meetingID: meetingID)
    let master = try CAFFile.read(layout.master(.caf48kFloat32))
    #expect(master.channels.count == 1)
    #expect(abs(master.duration - 0.5) < 0.1)
    let sidecar = try WAVAudioDecoder.read(layout.sidecar(.mixed))
    #expect(abs(sidecar.duration - 0.5) < 0.1)
    #expect(!FileManager.default.fileExists(atPath: layout.sidecar(.mic).path))
  }

  @Test func recordUsageErrors() throws {
    let home = try Fixtures.temporaryDirectory("steno-home")
    defer { try? FileManager.default.removeItem(at: home) }
    let badMode = try CLITests.run(["record", "--mode", "phone", "--out", home.path], home: home)
    #expect(badMode.status == 1)
    let badID = try CLITests.run(
      ["record", "--out", home.path, "--meeting-id", "nope", "--backend", "synthetic"], home: home)
    #expect(badID.status == 1)
    #expect(badID.stderr.contains("UUID"))
  }
}
