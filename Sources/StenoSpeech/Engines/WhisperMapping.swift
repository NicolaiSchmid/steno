import Foundation
import StenoCore

/// One WhisperKit segment as the engine reads it: the framework result is
/// flattened into these first, so the language decision and the mapping to
/// `RawSegment`s are tested without the framework.
struct WhisperSegment: Sendable, Equatable {
  var start: TimeInterval
  var end: TimeInterval
  var text: String
  var noSpeechProb: Float
  var words: [TimedWord]

  init(
    start: TimeInterval, end: TimeInterval, text: String, noSpeechProb: Float = 0,
    words: [TimedWord] = []
  ) {
    self.start = start
    self.end = end
    self.text = text
    self.noSpeechProb = noSpeechProb
    self.words = words
  }
}

/// Everything `WhisperKitEngine` decides around the model call: which
/// language to pin, and how the model's segments become tagged
/// `RawSegment`s.
struct WhisperMapping: Sendable {
  static let noSpeechThreshold: Float = 0.6

  var tagger = LanguageTagger()
  var noSpeechThreshold: Float = WhisperMapping.noSpeechThreshold

  init(tagger: LanguageTagger = LanguageTagger(), noSpeechThreshold: Float = 0.6) {
    self.tagger = tagger
    self.noSpeechThreshold = noSpeechThreshold
  }

  /// `en-US` becomes `en`; a language Whisper does not know gives nil so the
  /// model is not forced into a code it has no token for.
  static func whisperCode(for language: Locale.Language) -> String? {
    guard let code = LanguageTag(language).rawValue.split(separator: "-").first else {
      return nil
    }
    let tag = LanguageTag(rawValue: String(code))
    return SpeechEngineID.whisperKitLargeV3Turbo.supportedLanguageTags.contains(tag)
      ? tag.rawValue : nil
  }

  /// The language Whisper is pinned to for the whole buffer: the hint's code
  /// when Whisper knows it, otherwise the majority of `detect` over the most
  /// energetic windows (`WhisperWindowRanking`), nil when nothing decides.
  /// `detect` is never called when the hint settles it.
  static func pinnedLanguage(
    samples: [Float], hint: Locale.Language?,
    detect: @Sendable (ArraySlice<Float>) async throws -> String
  ) async throws -> String? {
    if let hint, let code = whisperCode(for: hint) { return code }
    var votes: [String] = []
    for window in WhisperWindowRanking.topWindows(samples: samples) {
      votes.append(try await detect(samples[window.samples]))
    }
    return WhisperWindowRanking.majority(votes)
  }

  /// Drops segments the model considers silence (`noSpeechProb` above the
  /// threshold) and empty ones, trims words, orders by start and tags the
  /// language with the pinned code as the hint.
  func segments(from segments: [WhisperSegment], pinned: String?) -> [RawSegment] {
    var mapped: [RawSegment] = []
    for segment in segments {
      guard segment.noSpeechProb <= noSpeechThreshold else { continue }
      let text = segment.text.trimmingCharacters(in: .whitespacesAndNewlines)
      guard !text.isEmpty else { continue }
      let words = segment.words.compactMap { word -> WordTiming? in
        let trimmed = word.text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        return WordTiming(word: trimmed, start: word.start, end: word.end)
      }
      mapped.append(
        RawSegment(
          start: segment.start, end: max(segment.start, segment.end), text: text,
          wordTimings: words.isEmpty ? nil : words))
    }
    mapped.sort { $0.start < $1.start }
    return tagger.tag(mapped, hint: pinned.map { LanguageTag(rawValue: $0) })
  }
}
