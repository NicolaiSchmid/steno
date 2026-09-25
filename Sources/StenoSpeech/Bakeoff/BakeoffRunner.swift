import Foundation
import StenoCore

/// Runs every requested engine over every audio file in a folder and
/// measures wall clock, WER against `<name>.ref.txt`, language flips and,
/// with a cleaner injected, the WER after cleanup. Decoding is injected too:
/// StenoAudio's codec in the CLI for m4a and CAF, core's WAV reader in tests.
public struct BakeoffRunner: Sendable {
  static let audioExtensions: Set<String> = ["wav", "m4a", "mp3", "caf"]

  /// Builds the engine for an id; the CLI passes `makeSpeechEngine` over its
  /// store, tests pass fakes.
  public var makeEngine: @Sendable (SpeechEngineID) throws -> any SpeechEngine
  public var decoder: any AudioDecoder
  public var cleaner: (any TranscriptCleaner)?
  var tagger = LanguageTagger()
  var clock = ContinuousClock()

  public init(
    makeEngine: @escaping @Sendable (SpeechEngineID) throws -> any SpeechEngine,
    decoder: any AudioDecoder = WAVAudioDecoder(),
    cleaner: (any TranscriptCleaner)? = nil
  ) {
    self.makeEngine = makeEngine
    self.decoder = decoder
    self.cleaner = cleaner
  }

  /// Audio files of the folder, sorted by name.
  static func audioFiles(in directory: URL) throws -> [URL] {
    try FileManager.default.contentsOfDirectory(
      at: directory, includingPropertiesForKeys: nil, options: [.skipsHiddenFiles]
    )
    .filter { audioExtensions.contains($0.pathExtension.lowercased()) }
    .sorted { $0.lastPathComponent < $1.lastPathComponent }
  }

  /// `<name>.ref.txt` beside the audio or in `referenceDirectory`.
  static func reference(for audio: URL, referenceDirectory: URL?) -> String? {
    let name = audio.deletingPathExtension().lastPathComponent + ".ref.txt"
    let url = (referenceDirectory ?? audio.deletingLastPathComponent()).appendingPathComponent(name)
    return try? String(contentsOf: url, encoding: .utf8)
  }

  /// Runs the bake-off. With `output` set, writes `report.md`, `report.json`
  /// and `<name>.<engine>.json` (the raw segments) into it.
  public func run(
    audioDirectory: URL, referenceDirectory: URL? = nil, engines: [SpeechEngineID],
    output: URL? = nil
  ) async throws -> BakeoffReport {
    let files = try Self.audioFiles(in: audioDirectory)
    if let output {
      try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
    }
    var rows: [BakeoffRow] = []
    for engineID in engines {
      let engine = try makeEngine(engineID)
      try await engine.prepare()
      // One untimed pass over the first file: the first transcription of a
      // freshly loaded model carries the CoreML and ANE compile, which would
      // otherwise land on whichever file sorts first and make its wall clock
      // incomparable with the rest of the column.
      if let first = files.first {
        _ = try await engine.transcribe(
          try await decoder.decode(Self.asset(for: first), lane: .mixed), hint: nil)
      }
      for file in files {
        let audio = try await decoder.decode(Self.asset(for: file), lane: .mixed)
        let started = clock.now
        let segments = try await engine.transcribe(audio, hint: nil)
        let elapsed = clock.now - started
        let reference = Self.reference(for: file, referenceDirectory: referenceDirectory)
        let hypothesis = segments.map(\.text).joined(separator: " ")
        var row = BakeoffRow(
          file: file.lastPathComponent, engine: engineID, audioSeconds: audio.duration,
          wallSeconds: elapsed / .seconds(1), segmentCount: segments.count,
          wer: reference.map { WordErrorRate.compute(reference: $0, hypothesis: hypothesis) },
          languageFlips: tagger.languageFlips(in: segments),
          dominantLanguage: tagger.dominantLanguage(of: segments))
        if let cleaner, let reference {
          let cleaned = try await Self.cleaned(segments, with: cleaner, tagger: tagger)
          row.cleanedWER = WordErrorRate.compute(reference: reference, hypothesis: cleaned)
        }
        rows.append(row)
        if let output {
          let name = file.deletingPathExtension().lastPathComponent
          try StenoJSON.encode(segments)
            .write(to: output.appendingPathComponent("\(name).\(engineID.rawValue).json"))
        }
      }
    }
    let report = BakeoffReport(rows: rows)
    if let output {
      try Data(report.markdown().utf8).write(to: output.appendingPathComponent("report.md"))
      try report.json().write(to: output.appendingPathComponent("report.json"))
    }
    return report
  }

  /// A stand-alone file wrapped as a one-lane asset so `AudioDecoder` can
  /// read it.
  static func asset(for file: URL) -> AudioAsset {
    let format: AudioFormat =
      switch file.pathExtension.lowercased() {
      case "wav": .wav16kInt16
      case "caf": .caf48kFloat32
      default: .m4aAAC
      }
    return AudioAsset(
      id: UUID(), meetingID: UUID(), url: file, format: format, lanes: [.mixed],
      retention: .keepForever)
  }

  static func cleaned(
    _ segments: [RawSegment], with cleaner: any TranscriptCleaner, tagger: LanguageTagger
  ) async throws -> String {
    let meetingID = UUID()
    let transcript = segments.enumerated().map { index, segment in
      TranscriptSegment(
        id: UUID(derivedFrom: meetingID, salt: "segment-mixed-\(index)"), meetingID: meetingID,
        start: segment.start, end: segment.end, lane: .mixed, text: segment.text,
        rawText: segment.text)
    }
    let output = try await cleaner.clean(
      CleanupInput(
        segments: transcript, language: tagger.dominantLanguage(of: segments), participants: [],
        speakers: [], knownPeople: []))
    return output.segments.map(\.text).joined(separator: " ")
  }
}
