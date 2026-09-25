import ArgumentParser
import Foundation
import StenoAudio
import StenoCore
import StenoLLM
import StenoSpeech

extension SpeechEngineID: ExpressibleByArgument {}

/// `steno dev bakeoff <audio-dir> [--engines] [--reference-dir] [--cleanup]
/// [--json] [--out]`: runs the requested engines over a folder of recordings and
/// writes `report.md`, `report.json` and the raw segments per file and
/// engine. Models download on first use. Input is any `wav|m4a|mp3|caf` file
/// `AVFoundationAudioCodec` reads: channel 0 is resampled to 16 kHz mono.
/// `--cleanup` sends every transcript that has a reference through
/// `LLMTranscriptCleaner` on the configured endpoint and reports the WER
/// after it; text only, the audio never leaves the machine.
struct DevBakeoff: AsyncParsableCommand {
  static let configuration = CommandConfiguration(
    commandName: "bakeoff",
    abstract: "Compare speech engines over a folder of recordings.")

  @Argument(
    help: "Folder with wav, m4a, mp3 or caf recordings and optional <name>.ref.txt references.")
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

  @Flag(
    help:
      "Run the LLM cleanup pass (Settings.llmBaseURL and llmModel) over each transcript and report the WER after it."
  )
  var cleanup = false

  @Flag(help: "Print the report as JSON (the contents of report.json) instead of Markdown.")
  var json = false

  /// Runs core's `FakeSpeechEngine` under every requested id, so the CLI
  /// tests exercise decoding, reporting and the LLM wiring without a model
  /// download. Hidden: it measures nothing.
  @Flag(name: .customLong("fake-engines"), help: .hidden)
  var fakeEngines = false

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
    let audio = URL(fileURLWithPath: audioDirectory, isDirectory: true)
    let out =
      output.map { URL(fileURLWithPath: $0, isDirectory: true) }
      ?? audio.appendingPathComponent("bakeoff", isDirectory: true)
    let runner = BakeoffRunner(
      makeEngine: try await makeEngine(), decoder: Self.decoder,
      cleaner: cleanup ? try await makeCleaner() : nil)
    let report: BakeoffReport
    do {
      report = try await runner.run(
        audioDirectory: audio,
        referenceDirectory: referenceDirectory.map { URL(fileURLWithPath: $0, isDirectory: true) },
        engines: engines,
        output: out)
    } catch let error as LLMError {
      throw RuntimeFailure(description: "cleanup failed: \(error)")
    }
    if json {
      print(String(decoding: try report.json(), as: UTF8.self))
      return
    }
    print(report.markdown())
    print("reports: \(out.path)")
  }

  /// The real cleaner from the stored settings, or nil with a notice on
  /// stderr when no endpoint is configured: the bake-off still runs, the
  /// cleaned column stays empty.
  func makeCleaner() async throws -> LLMTranscriptCleaner? {
    let settings = try await Wiring.open(models.database).settings.load()
    guard let passes = try await Wiring.llmComponents(settings: settings) else {
      FileHandle.standardError.write(
        Data(
          "cleanup skipped: no LLM endpoint configured (set Settings.llmBaseURL and llmModel)\n"
            .utf8))
      return nil
    }
    return passes.cleaner
  }

  /// The real engines over the model store, or the fake behind
  /// `--fake-engines` (which then never opens the store).
  func makeEngine() async throws -> @Sendable (SpeechEngineID) throws -> any SpeechEngine {
    if fakeEngines {
      return { id in FakeSpeechEngine(id: id.rawValue, language: "de") }
    }
    let store = try await models.store()
    return { try makeSpeechEngine($0, models: store) }
  }

  /// StenoAudio's codec where AVFoundation exists (any sample rate, CAF,
  /// m4a, mp3); core's 16 kHz WAV reader elsewhere.
  static var decoder: any AudioDecoder {
    #if canImport(AVFoundation)
      AVFoundationAudioCodec()
    #else
      WAVAudioDecoder()
    #endif
  }
}
