import ArgumentParser
import Foundation
import StenoCore
import StenoSpeech

extension SpeechEngineID: ExpressibleByArgument {
  public static var allValueStrings: [String] { allCases.map(\.rawValue) }
}

/// `steno dev bakeoff <audio-dir> [--engines] [--reference-dir] [--out]`:
/// runs the requested engines over a folder of recordings and writes
/// `report.md`, `report.json` and the raw segments per file and engine.
/// Models download on first use. Input is 16 kHz mono WAV until StenoAudio's
/// codec lands; `--cleanup` arrives with StenoLLM's cleaner.
struct DevBakeoff: AsyncParsableCommand {
  static let configuration = CommandConfiguration(
    commandName: "bakeoff",
    abstract: "Compare speech engines over a folder of recordings.")

  @Argument(help: "Folder with 16 kHz mono WAV files and optional <name>.ref.txt references.")
  var audioDirectory: String

  @Option(
    parsing: .upToNextOption,
    help: "Engines to run: \(SpeechEngineID.allValueStrings.joined(separator: ", ")).")
  var engines: [SpeechEngineID] = [.parakeetV3]

  @Option(
    name: .customLong("reference-dir"),
    help: "Folder with <name>.ref.txt; defaults to the audio folder.")
  var referenceDirectory: String?

  @Option(name: .customLong("out"), help: "Where the reports go; defaults to <audio-dir>/bakeoff.")
  var output: String?

  @OptionGroup var models: DevModels.Options

  func validate() throws {
    var isDirectory: ObjCBool = false
    guard FileManager.default.fileExists(atPath: audioDirectory, isDirectory: &isDirectory),
      isDirectory.boolValue
    else {
      throw ValidationError("No such folder: \(audioDirectory)")
    }
  }

  func run() async throws {
    let store = try await models.store()
    let audio = URL(fileURLWithPath: audioDirectory, isDirectory: true)
    let out =
      output.map { URL(fileURLWithPath: $0, isDirectory: true) }
      ?? audio.appendingPathComponent("bakeoff", isDirectory: true)
    let runner = BakeoffRunner(engineProvider: { try makeSpeechEngine($0, models: store) })
    let report = try await runner.run(
      audioDirectory: audio,
      referenceDirectory: referenceDirectory.map { URL(fileURLWithPath: $0, isDirectory: true) },
      engines: engines,
      output: out)
    print(report.markdown())
    print("reports: \(out.path)")
  }
}
