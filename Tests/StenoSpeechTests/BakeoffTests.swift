import Foundation
import StenoCore
import Testing

@testable import StenoSpeech

@Suite struct WordErrorRateTests {
  @Test func knownPairs() {
    #expect(WordErrorRate.compute(reference: "guten morgen", hypothesis: "guten morgen") == 0)
    #expect(WordErrorRate.compute(reference: "guten morgen", hypothesis: "guten abend") == 0.5)
    #expect(WordErrorRate.compute(reference: "a b c d", hypothesis: "a c d") == 0.25, "deletion")
    #expect(
      WordErrorRate.compute(reference: "a b c d", hypothesis: "a b x c d") == 0.25, "insertion")
    #expect(WordErrorRate.compute(reference: "a b", hypothesis: "") == 1)
    #expect(WordErrorRate.compute(reference: "", hypothesis: "") == 0)
    #expect(WordErrorRate.compute(reference: "", hypothesis: "x") == 1)
    #expect(WordErrorRate.compute(reference: "a", hypothesis: "b c d") == 3, "can exceed one")
  }

  @Test func normalisationIgnoresCasePunctuationAndUmlauts() {
    #expect(
      WordErrorRate.normalise("Hallo, Welt! Über 3 Straßen.") == [
        "hallo", "welt", "ueber", "3", "strassen",
      ])
    #expect(WordErrorRate.normalise("Über", foldUmlauts: false) == ["über"])
    #expect(WordErrorRate.compute(reference: "München", hypothesis: "Muenchen") == 0)
    #expect(
      WordErrorRate.compute(reference: "München", hypothesis: "Muenchen", foldUmlauts: false) == 1)
  }
}

@Suite struct BakeoffReportTests {
  private let rows = [
    BakeoffRow(
      file: "de-short.wav", engine: .parakeetV3, audioSeconds: 5, wallSeconds: 0.5, segmentCount: 1,
      wer: 0.1, languageFlips: 0, dominantLanguage: "de", cleanedWER: 0.05),
    BakeoffRow(
      file: "en-short.wav", engine: .parakeetV3, audioSeconds: 4, wallSeconds: 0.25,
      segmentCount: 2,
      wer: nil, languageFlips: 1, dominantLanguage: "en"),
    BakeoffRow(
      file: "de-short.wav", engine: .whisperKitLargeV3Turbo, audioSeconds: 5, wallSeconds: 2,
      segmentCount: 1, wer: 0.2, languageFlips: 0, dominantLanguage: "de"),
  ]

  @Test func markdownHasARowPerFileAndASummaryPerEngine() {
    let markdown = BakeoffReport(rows: rows).markdown()
    let lines = markdown.split(separator: "\n").map(String.init)
    #expect(lines.first == "# STT bake-off")
    #expect(lines.filter { $0.hasPrefix("| de-short.wav") }.count == 2)
    #expect(
      lines.contains {
        $0.hasPrefix("| en-short.wav | parakeet-v3 | 4.00 | 0.25 | 16.00 | 2 | - | - | 1 | en |")
      })
    #expect(lines.contains { $0.hasPrefix("| parakeet-v3 | 2 | 13.00 | 10.0 % | 5.0 % | 1 |") })
    #expect(
      lines.contains { $0.hasPrefix("| whisperkit-large-v3-turbo | 1 | 2.50 | 20.0 % | - | 0 |") })
  }

  @Test func jsonRoundTrips() throws {
    let report = BakeoffReport(rows: rows, generatedAt: Date(timeIntervalSince1970: 1_700_000_000))
    let decoder = JSONDecoder()
    decoder.dateDecodingStrategy = .iso8601
    let decoded = try decoder.decode(BakeoffReport.self, from: report.json())
    #expect(decoded == report)
    #expect(
      String(decoding: try report.json(), as: UTF8.self).contains("\"engine\" : \"parakeet-v3\""))
  }

  @Test func realtimeFactorNeverDividesByZero() {
    let row = BakeoffRow(
      file: "x", engine: .parakeetV3, audioSeconds: 1, wallSeconds: 0, segmentCount: 0,
      languageFlips: 0)
    #expect(row.realtimeFactor.isFinite)
  }
}

@Suite struct BakeoffRunnerTests {
  @Test func runsEveryEngineOverEveryFileAndWritesReports() async throws {
    let directory = try Fixtures.temporaryDirectory("bakeoff")
    defer { try? FileManager.default.removeItem(at: directory) }
    let audio = directory.appendingPathComponent("audio", isDirectory: true)
    try FileManager.default.createDirectory(at: audio, withIntermediateDirectories: true)
    for name in ["sweep-3s.wav", "noise-2s.wav"] {
      try FileManager.default.copyItem(
        at: Fixtures.url("audio/\(name)"), to: audio.appendingPathComponent(name))
    }
    try Data("fake segment 1 fake segment 2 fake segment 3".utf8)
      .write(to: audio.appendingPathComponent("sweep-3s.ref.txt"))
    try Data("notes".utf8).write(to: audio.appendingPathComponent("ignored.txt"))

    let german = FakeSpeechEngine(id: "parakeet-v3", language: "de")
    let english = FakeSpeechEngine(id: "whisperkit-large-v3-turbo", language: "en")
    let runner = BakeoffRunner(
      engineProvider: { id in id == .parakeetV3 ? german : english },
      cleaner: PassthroughCleaner())
    let output = directory.appendingPathComponent("out", isDirectory: true)
    let report = try await runner.run(
      audioDirectory: audio, engines: [.parakeetV3, .whisperKitLargeV3Turbo], output: output)

    #expect(report.rows.count == 4)
    #expect(
      report.rows.map(\.file) == ["noise-2s.wav", "sweep-3s.wav", "noise-2s.wav", "sweep-3s.wav"])
    #expect(
      report.rows.map(\.engine) == [
        .parakeetV3, .parakeetV3, .whisperKitLargeV3Turbo, .whisperKitLargeV3Turbo,
      ])
    let sweep = report.rows[1]
    #expect(sweep.audioSeconds == 3)
    #expect(sweep.segmentCount == 3)
    #expect(sweep.wer == 0)
    #expect(sweep.cleanedWER == 0)
    #expect(sweep.languageFlips == 0)
    #expect(sweep.dominantLanguage == "de")
    #expect(report.rows[0].wer == nil, "no reference for the noise file")
    #expect(report.rows[3].dominantLanguage == "en")
    #expect(await german.preparations.count == 1)
    #expect(await english.transcriptions.count == 2)

    let written = try FileManager.default.contentsOfDirectory(atPath: output.path).sorted()
    #expect(
      written == [
        "noise-2s.parakeet-v3.json", "noise-2s.whisperkit-large-v3-turbo.json", "report.json",
        "report.md", "sweep-3s.parakeet-v3.json", "sweep-3s.whisperkit-large-v3-turbo.json",
      ])
    let segments = try StenoJSON.decode(
      [RawSegment].self,
      from: Data(contentsOf: output.appendingPathComponent("sweep-3s.parakeet-v3.json")))
    #expect(segments.count == 3)
    #expect(
      try String(contentsOf: output.appendingPathComponent("report.md"), encoding: .utf8).contains(
        "| sweep-3s.wav | parakeet-v3 |"))
  }

  /// A cleaner that rewrites the transcript to `replacement`: `cleanedWER`
  /// must be measured on its output, and only where a reference exists.
  struct RewritingCleaner: TranscriptCleaner, Sendable {
    var replacement: String
    func clean(_ input: CleanupInput) async throws -> CleanupOutput {
      var segments = input.segments
      for index in segments.indices { segments[index].text = index == 0 ? replacement : "" }
      return CleanupOutput(
        segments: segments, usage: LLMUsage(promptTokens: 1, completionTokens: 1, requests: 1))
    }
  }

  @Test func cleanedWERIsMeasuredOnTheCleanersOutputOnlyWithAReference() async throws {
    let directory = try Fixtures.temporaryDirectory("bakeoff-clean")
    defer { try? FileManager.default.removeItem(at: directory) }
    for name in ["sweep-3s.wav", "noise-2s.wav"] {
      try FileManager.default.copyItem(
        at: Fixtures.url("audio/\(name)"), to: directory.appendingPathComponent(name))
    }
    try Data("richtig gesagt".utf8).write(to: directory.appendingPathComponent("sweep-3s.ref.txt"))
    let runner = BakeoffRunner(
      engineProvider: { _ in FakeSpeechEngine(id: "parakeet-v3", language: "de") },
      cleaner: RewritingCleaner(replacement: "richtig gesagt"))
    let report = try await runner.run(audioDirectory: directory, engines: [.parakeetV3])
    let sweep = try #require(report.rows.first { $0.file == "sweep-3s.wav" })
    #expect(sweep.wer ?? 0 > 1, "the fake's nine words against two reference words")
    #expect(sweep.cleanedWER == 0)
    let noise = try #require(report.rows.first { $0.file == "noise-2s.wav" })
    #expect(noise.wer == nil && noise.cleanedWER == nil)
  }

  @Test func languageFlipsCountAdjacentTaggedSegmentsOnly() {
    func segment(_ language: LanguageTag?) -> RawSegment {
      RawSegment(start: 0, end: 1, text: "", language: language)
    }
    #expect(LanguageTagger.languageFlips(in: [segment("de"), segment("en"), segment("de")]) == 2)
    #expect(
      LanguageTagger.languageFlips(in: [segment("de"), segment(nil), segment("de")]) == 0,
      "untagged segments do not flip")
    #expect(LanguageTagger.languageFlips(in: [segment("de"), segment(nil), segment("en")]) == 1)
    #expect(LanguageTagger.languageFlips(in: [segment("de")]) == 0)
    #expect(
      LanguageTagger.languageFlips(in: [segment("en-US"), segment("en")]) == 1,
      "tags compare verbatim")
  }

  @Test func anEngineFailureAbortsTheRunWithItsError() async throws {
    struct Boom: Error, Equatable {}
    let directory = try Fixtures.temporaryDirectory("bakeoff-fail")
    defer { try? FileManager.default.removeItem(at: directory) }
    try FileManager.default.copyItem(
      at: Fixtures.url("audio/sweep-3s.wav"), to: directory.appendingPathComponent("sweep-3s.wav"))
    let runner = BakeoffRunner(engineProvider: { _ in FakeSpeechEngine(failure: Boom()) })
    await #expect(throws: Boom.self) {
      _ = try await runner.run(audioDirectory: directory, engines: [.parakeetV3])
    }
    #expect(throws: (any Error).self) {
      _ = try BakeoffRunner.audioFiles(in: directory.appendingPathComponent("missing"))
    }
  }

  @Test func referenceLookupPrefersTheReferenceDirectory() throws {
    let directory = try Fixtures.temporaryDirectory("refs")
    defer { try? FileManager.default.removeItem(at: directory) }
    let audio = directory.appendingPathComponent("a.wav")
    try Data("beside".utf8).write(to: directory.appendingPathComponent("a.ref.txt"))
    let refs = directory.appendingPathComponent("refs", isDirectory: true)
    try FileManager.default.createDirectory(at: refs, withIntermediateDirectories: true)
    try Data("elsewhere".utf8).write(to: refs.appendingPathComponent("a.ref.txt"))
    #expect(BakeoffRunner.reference(for: audio, referenceDirectory: nil) == "beside")
    #expect(BakeoffRunner.reference(for: audio, referenceDirectory: refs) == "elsewhere")
    #expect(
      BakeoffRunner.reference(
        for: directory.appendingPathComponent("b.wav"), referenceDirectory: nil) == nil)
    #expect(BakeoffRunner.asset(for: audio).format == .wav16kInt16)
    #expect(BakeoffRunner.asset(for: directory.appendingPathComponent("b.m4a")).format == .m4aAAC)
    #expect(
      BakeoffRunner.asset(for: directory.appendingPathComponent("c.caf")).format == .caf48kFloat32)
  }
}

@Suite struct WhisperWindowRankingTests {
  @Test func ranksWindowsByEnergy() {
    // Three 30 s windows at 16 kHz plus a 10 s tail: quiet, loud, medium, silent tail.
    let rate = 16_000
    let quiet = [Float](repeating: 0.01, count: 30 * rate)
    let loud = [Float](repeating: 0.5, count: 30 * rate)
    let medium = [Float](repeating: 0.1, count: 30 * rate)
    let tail = [Float](repeating: 0, count: 10 * rate)
    let windows = WhisperWindowRanking.topWindows(samples: quiet + loud + medium + tail, count: 3)
    #expect(windows.map(\.samples.lowerBound) == [30 * rate, 60 * rate, 0])
    #expect(windows.first?.rms ?? 0 > 0.49)
    #expect(WhisperWindowRanking.topWindows(samples: [], count: 3).isEmpty)
    #expect(WhisperWindowRanking.topWindows(samples: quiet, count: 0).isEmpty)
  }

  @Test func shortAudioIsOneWindow() {
    let windows = WhisperWindowRanking.topWindows(samples: [Float](repeating: 0.2, count: 16_000))
    #expect(windows.count == 1)
    #expect(windows[0].samples == 0..<16_000)
    #expect(abs(windows[0].rms - 0.2) < 1e-5)
  }

  @Test func majorityVoteWithTiesToTheFirst() {
    #expect(WhisperWindowRanking.majority(["de", "en", "de"]) == "de")
    #expect(WhisperWindowRanking.majority(["en", "de"]) == "en")
    #expect(WhisperWindowRanking.majority([]) == nil)
  }
}
