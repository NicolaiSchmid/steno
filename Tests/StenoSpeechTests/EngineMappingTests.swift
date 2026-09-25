import Foundation
import StenoCore
import Testing

@testable import StenoSpeech

/// The chain behind `ParakeetEngine.transcribe` after the model returns:
/// token timings become words, words become segments, segments get a
/// language. Everything the engine promises about `RawSegment` timing is
/// decided here, so it is pinned without a model.
@Suite struct ParakeetMappingTests {
  private let mapping = ParakeetMapping(
    tagger: LanguageTagger(recognizer: StopwordLanguageRecognizer()))

  private func token(_ text: String, _ start: TimeInterval, _ end: TimeInterval, _ c: Float = 1)
    -> TimedWord
  {
    TimedWord(text: text, start: start, end: end, confidence: c)
  }

  @Test func tokensBecomeTimedSegmentsWithWordTimings() throws {
    // "Wir haben das heute nicht besprochen." then, after a 1.5 s pause,
    // "We should ship the flow this week." The second sentence is English.
    // Tokens as FluidAudio delivers them: the `▁` marker already a space.
    let tokens = [
      token(" Wir", 0.00, 0.20), token(" haben", 0.25, 0.50), token(" das", 0.55, 0.70),
      token(" heute", 0.75, 1.00), token(" nicht", 1.05, 1.30), token(" be", 1.35, 1.50),
      token("sprochen", 1.50, 1.90), token(".", 1.90, 1.95),
      token(" We", 3.50, 3.60), token(" should", 3.65, 3.90), token(" ship", 3.95, 4.10),
      token(" the", 4.15, 4.25), token(" flow", 4.30, 4.50), token(" this", 4.55, 4.70),
      token(" week", 4.75, 5.00), token(".", 5.00, 5.05),
    ]
    let segments = mapping.segments(tokens: tokens, text: "ignored", duration: 6, hint: nil)
    try #require(segments.count == 2)
    #expect(segments[0].text == "Wir haben das heute nicht besprochen.")
    #expect(segments[1].text == "We should ship the flow this week.")
    #expect(segments[0].start == 0 && segments[0].end == 1.95)
    #expect(segments[1].start == 3.5 && segments[1].end == 5.05)
    #expect(segments.map(\.language) == ["de", "en"])
    let words = try #require(segments[0].wordTimings)
    #expect(words.map(\.word) == ["Wir", "haben", "das", "heute", "nicht", "besprochen."])
    #expect(words[5].start == 1.35 && words[5].end == 1.95, "a word spans its tokens")
    // Segments never overlap and never run backwards.
    for (lhs, rhs) in zip(segments, segments.dropFirst()) { #expect(lhs.end <= rhs.start) }
    #expect(segments.allSatisfy { $0.start <= $0.end })
  }

  @Test func textWithoutTokenTimingsBecomesOneSegmentOverTheWholeBuffer() {
    let segments = mapping.segments(
      tokens: [], text: "  Wir haben das heute nicht besprochen. \n", duration: 4.2, hint: nil)
    #expect(
      segments == [
        RawSegment(
          start: 0, end: 4.2, text: "Wir haben das heute nicht besprochen.", language: "de")
      ])
  }

  @Test func nothingRecognisedGivesNoSegments() {
    #expect(mapping.segments(tokens: [], text: "   ", duration: 3, hint: nil).isEmpty)
    #expect(
      mapping.segments(tokens: [token(" ", 0, 0.1)], text: "", duration: 3, hint: nil).isEmpty)
    #expect(
      mapping.segments(tokens: [token("▁", 0, 0.1)], text: "", duration: 3, hint: nil).isEmpty)
  }

  @Test func theHintOnlySteersTheTaggerAndCoversUndecidableText() {
    let tokens = [token(" Kubernetes", 0, 0.5), token(" Grafana", 0.6, 1.0)]
    let german = mapping.segments(
      tokens: tokens, text: "", duration: 1, hint: LanguageTag("de").language)
    #expect(german.map(\.language) == ["de"])
    let english = mapping.segments(
      tokens: tokens, text: "", duration: 1, hint: LanguageTag("en-US").language)
    #expect(english.map(\.language) == ["en-US"], "the hint is passed through as given")
    let unhinted = mapping.segments(tokens: tokens, text: "", duration: 1, hint: nil)
    #expect(unhinted.map(\.language) == [nil])
    // Clear text wins over the hint.
    let clear = mapping.segments(
      tokens: [
        token(" We", 0, 0.1), token(" should", 0.2, 0.4), token(" ship", 0.5, 0.6),
        token(" the", 0.7, 0.8), token(" flow", 0.9, 1.0),
      ], text: "", duration: 1, hint: LanguageTag("de").language)
    #expect(clear.map(\.language) == ["en"])
  }
}

/// What `WhisperKitEngine` decides around the model call: the language it
/// pins and how the model's segments become `RawSegment`s.
@Suite struct WhisperMappingTests {
  private let mapping = WhisperMapping(
    tagger: LanguageTagger(recognizer: StopwordLanguageRecognizer()))

  @Test func hintIsReducedToWhispersTwoLetterCode() {
    #expect(WhisperMapping.whisperCode(for: LanguageTag("de").language) == "de")
    #expect(WhisperMapping.whisperCode(for: LanguageTag("en-US").language) == "en")
    #expect(WhisperMapping.whisperCode(for: LanguageTag("de-CH").language) == "de")
    #expect(WhisperMapping.whisperCode(for: LanguageTag("yue").language) == "yue")
    #expect(
      WhisperMapping.whisperCode(for: LanguageTag("tlh").language) == nil,
      "Klingon has no Whisper token")
  }

  @Test func aKnownHintPinsWithoutDetecting() async throws {
    let detections = CallLog<Int>()
    let pinned = try await WhisperMapping.pinnedLanguage(
      samples: [Float](repeating: 0.1, count: 16_000), hint: LanguageTag("de-AT").language
    ) { slice in
      await detections.record(slice.count)
      return "en"
    }
    #expect(pinned == "de")
    #expect(await detections.count == 0)
  }

  @Test func withoutAHintTheMostEnergeticWindowsVote() async throws {
    // Four 30 s windows: quiet, loud, medium, silent. The three loudest are
    // asked, the quiet one first among them is not the majority.
    let rate = 16_000
    let quiet = [Float](repeating: 0.01, count: 30 * rate)
    let loud = [Float](repeating: 0.5, count: 30 * rate)
    let medium = [Float](repeating: 0.1, count: 30 * rate)
    let silent = [Float](repeating: 0, count: 30 * rate)
    let samples = quiet + loud + medium + silent
    let asked = CallLog<Int>()
    let pinned = try await WhisperMapping.pinnedLanguage(samples: samples, hint: nil) { slice in
      await asked.record(slice.startIndex / rate)
      // Loud window says German, the other two English: majority English.
      return slice.startIndex == 30 * rate ? "de" : "en"
    }
    #expect(pinned == "en")
    #expect(await asked.entries == [30, 60, 0], "loudest first, the silent window never asked")

    // An unknown hint falls back to detection too.
    let unknownHint = try await WhisperMapping.pinnedLanguage(
      samples: loud, hint: LanguageTag("tlh").language
    ) { _ in "de" }
    #expect(unknownHint == "de")
    let nothing = try await WhisperMapping.pinnedLanguage(samples: [], hint: nil) { _ in "de" }
    #expect(nothing == nil, "no audio, no vote")
  }

  @Test func detectionFailuresSurface() async {
    struct Boom: Error {}
    await #expect(throws: Boom.self) {
      try await WhisperMapping.pinnedLanguage(
        samples: [Float](repeating: 0.2, count: 16_000), hint: nil
      ) { _ in throw Boom() }
    }
  }

  @Test func silentAndEmptySegmentsAreDroppedAndTheRestOrderedAndTagged() throws {
    let segments = [
      WhisperSegment(
        start: 5, end: 7.5, text: " We should ship the flow this week. ", noSpeechProb: 0.1,
        words: [
          TimedWord(text: " We", start: 5, end: 5.2),
          TimedWord(text: " should", start: 5.3, end: 5.6),
          TimedWord(text: "  ", start: 5.6, end: 5.7),
        ]),
      WhisperSegment(
        start: 0, end: 4, text: "Wir haben das heute nicht besprochen.", noSpeechProb: 0),
      WhisperSegment(start: 8, end: 9, text: "(silence)", noSpeechProb: 0.61),
      WhisperSegment(start: 9, end: 10, text: "   ", noSpeechProb: 0),
      WhisperSegment(start: 12, end: 11, text: "Genau", noSpeechProb: 0.6),
    ]
    let mapped = mapping.segments(from: segments, pinned: nil)
    #expect(
      mapped.map(\.text) == [
        "Wir haben das heute nicht besprochen.", "We should ship the flow this week.", "Genau",
      ])
    #expect(mapped.map(\.start) == [0, 5, 12])
    #expect(mapped[2].end == 12, "an end before the start is clamped")
    #expect(mapped.map(\.language) == ["de", "en", "en"], "the short segment inherits")
    let words = try #require(mapped[1].wordTimings)
    #expect(words.map(\.word) == ["We", "should"], "blank words are dropped, the rest trimmed")
    #expect(mapped[0].wordTimings == nil, "no words reported, none invented")
    #expect(mapped[2].wordTimings == nil)
  }

  @Test func thePinnedLanguageIsTheTaggersHint() {
    let mapped = mapping.segments(
      from: [WhisperSegment(start: 0, end: 2, text: "Kubernetes Grafana Prometheus Terraform")],
      pinned: "de")
    #expect(mapped.map(\.language) == ["de"])
    #expect(
      mapping.segments(
        from: [WhisperSegment(start: 0, end: 2, text: "Kubernetes Grafana Prometheus Terraform")],
        pinned: nil
      ).map(\.language) == [nil])
  }

  @Test func theNoSpeechThresholdIsTheEnginesConstant() {
    #expect(WhisperMapping.noSpeechThreshold == 0.6)
    let strict = WhisperMapping(noSpeechThreshold: 0.2)
    #expect(
      strict.segments(
        from: [WhisperSegment(start: 0, end: 1, text: "Ja", noSpeechProb: 0.3)], pinned: nil
      ).isEmpty)
    #expect(
      strict.segments(
        from: [WhisperSegment(start: 0, end: 1, text: "Ja", noSpeechProb: 0.2)], pinned: nil
      ).count == 1)
  }
}

/// `makeSpeechEngine`, `makeDiarizer` and the `SpeechEngineID` table the app
/// and the CLI drive them through.
@Suite struct SpeechEngineFactoryTests {
  private func makeStore() throws -> (ModelStore, URL) {
    let directory = try Fixtures.temporaryDirectory("factory")
    return (ModelStore(directory: directory, downloader: FakeModelDownloader()), directory)
  }

  @Test func unknownSettingsValuesAreRejectedWithTheKnownOnesListed() throws {
    let error = #expect(throws: StenoSpeechError.self) {
      _ = try SpeechEngineID(settingsValue: "parakeet-v9")
    }
    #expect(error == .unknownEngine("parakeet-v9"))
    let message = String(describing: error ?? .unknownEngine(""))
    for id in SpeechEngineID.allCases { #expect(message.contains(id.rawValue)) }
    #expect(try SpeechEngineID(settingsValue: "parakeet-v3") == .parakeetV3)
  }

  @Test func everyIDBuildsItsEngineLazilyOrFailsWhereTheFrameworksAreMissing() throws {
    let (store, directory) = try makeStore()
    defer { try? FileManager.default.removeItem(at: directory) }
    for id in SpeechEngineID.allCases {
      #if canImport(FluidAudio) && canImport(WhisperKit)
        let engine = try makeSpeechEngine(id, models: store)
        #expect(engine.id == id.rawValue)
        #expect(engine.supportedLanguages == id.supportedLanguages, "\(id)")
        _ = try makeDiarizer(models: store)
      #else
        let error = #expect(throws: StenoSpeechError.self) {
          _ = try makeSpeechEngine(id, models: store)
        }
        #expect(error == .unsupportedPlatform(id.asset))
        let diarizer = #expect(throws: StenoSpeechError.self) {
          _ = try makeDiarizer(models: store)
        }
        #expect(diarizer == .unsupportedPlatform(.offlineDiarizer))
      #endif
    }
    // Building an engine never touches the models: downloads happen in
    // `prepare`, so the settings pane can list engines offline.
    #expect(store.installedAssets().isEmpty)
    #expect(
      try FileManager.default.contentsOfDirectory(atPath: directory.path).isEmpty,
      "nothing written under the models root")
  }

  @Test func theEngineTableMatchesThePlan() {
    #expect(
      SpeechEngineID.allCases.map(\.rawValue) == [
        "parakeet-v3", "parakeet-ultra", "parakeet-de", "whisperkit-large-v3-turbo",
      ])
    #expect(SpeechEngineID.userSelectable == [.parakeetV3, .whisperKitLargeV3Turbo])
    #expect(SpeechEngineID.parakeetV3.asset == .parakeetV3)
    #expect(SpeechEngineID.parakeetUltra.asset == .parakeetUltra)
    #expect(SpeechEngineID.parakeetDE.asset == .parakeetDE)
    #expect(SpeechEngineID.whisperKitLargeV3Turbo.asset == .whisperLargeV3Turbo)
    #expect(SpeechEngineID.parakeetDE.supportedLanguageTags == ["de"], "German-only fine-tune")
    #expect(SpeechEngineID.parakeetV3.supportedLanguageTags.count == 25)
    #expect(
      SpeechEngineID.parakeetUltra.supportedLanguageTags
        == SpeechEngineID.parakeetV3.supportedLanguageTags)
    #expect(
      SpeechEngineID.whisperKitLargeV3Turbo.supportedLanguageTags.count == 100,
      "Whisper large-v3: the 99 of large-v2 plus Cantonese")
    for id in SpeechEngineID.allCases {
      #expect(id.supportedLanguageTags.contains("de"), "\(id)")
      #expect(id.supportedLanguages.count == id.supportedLanguageTags.count, "\(id)")
      #expect(id.supportedLanguages.contains(LanguageTag("de").language), "\(id)")
    }
    #expect(SpeechEngineID.parakeetV3.supportedLanguages.contains(LanguageTag("en").language))
    #expect(!SpeechEngineID.parakeetDE.supportedLanguages.contains(LanguageTag("en").language))
  }

  @Test func idsRoundTripThroughSettingsStrings() throws {
    for id in SpeechEngineID.allCases {
      #expect(SpeechEngineID(rawValue: id.rawValue) == id)
      let json = try JSONEncoder().encode([id])
      #expect(try JSONDecoder().decode([SpeechEngineID].self, from: json) == [id])
    }
    #expect(SpeechEngineID(rawValue: "Parakeet-V3") == nil, "ids are case-sensitive")
  }

  @Test func diarizerConfigDefaultsArePlanConstants() {
    let config = FluidDiarizerConfig.default
    #expect(config.clusteringThreshold == 0.6)
    #expect(config.minSpeakers == nil && config.maxSpeakers == nil)
    #expect(config == FluidDiarizerConfig(clusteringThreshold: 0.6))
  }
}
