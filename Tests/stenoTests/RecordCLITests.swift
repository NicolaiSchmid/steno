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

  /// Every dev tool and `record` reject bad arguments with exit 1 before any
  /// device or file is touched.
  @Test func devToolArgumentsAreValidatedBeforeAnyDeviceIsTouched() throws {
    let home = try Fixtures.temporaryDirectory("steno-home")
    defer { try? FileManager.default.removeItem(at: home) }
    let noInputs = try CLITests.run(["dev", "aec-bench"], home: home)
    #expect(noInputs.status == 1)
    #expect(noInputs.stderr.contains("--synthetic"))
    let badEngine = try CLITests.run(
      ["dev", "aec-bench", "--synthetic", "--engine", "webrtc"], home: home)
    #expect(badEngine.status == 1)
    let badLanes = try CLITests.run(
      ["dev", "capture-spike", "--lanes", "phone", "--out", home.path], home: home)
    #expect(badLanes.status == 1)
    let zeroSeconds = try CLITests.run(
      ["record", "--backend", "synthetic", "--seconds", "0", "--out", home.path], home: home)
    #expect(zeroSeconds.status == 1)
    #expect(zeroSeconds.stderr.contains("positive"))
    let badBackend = try CLITests.run(
      ["record", "--backend", "tape", "--out", home.path], home: home)
    #expect(badBackend.status == 1)
    #expect(try FileManager.default.contentsOfDirectory(atPath: home.path).isEmpty)
  }

  /// `steno dev aec-bench --synthetic` runs the built-in echo fixtures through
  /// both engines: passthrough cancels nothing, Speex reaches the plan's
  /// 20 dB after three seconds, and `--out` writes the processed lane.
  @Test func syntheticAECBenchReportsERLEForBothEngines() throws {
    let home = try Fixtures.temporaryDirectory("steno-home")
    defer { try? FileManager.default.removeItem(at: home) }
    let passthrough = try CLITests.run(
      ["dev", "aec-bench", "--synthetic", "--engine", "passthrough"], home: home)
    #expect(passthrough.status == 0, "\(passthrough.stderr)")
    #expect(passthrough.stdout.contains("engine: passthrough, tail 200 ms"))
    #expect(passthrough.stdout.contains("after 3 s 0.0 dB"))

    let out = home.appendingPathComponent("processed.wav")
    let speex = try CLITests.run(
      ["dev", "aec-bench", "--synthetic", "--tail-milliseconds", "100", "--out", out.path],
      home: home)
    #expect(speex.status == 0, "\(speex.stderr)")
    #expect(speex.stdout.contains("engine: speex, tail 100 ms"))
    let tail = speex.stdout.components(separatedBy: "after 3 s ").last ?? ""
    let steady = Float(tail.split(separator: " ").first ?? "") ?? 0
    #expect(steady >= 20, "ERLE after 3 s: \(steady) dB")
    let processed = try WAVFile.read(out)
    #expect(processed.sampleRate == 48_000)
    #expect(processed.frameCount == 6 * 48_000)
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
