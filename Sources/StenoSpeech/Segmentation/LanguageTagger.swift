import Foundation
import StenoCore

#if canImport(NaturalLanguage)
  import NaturalLanguage
#endif

/// Decides the language of one piece of text among `candidates`. `hint` is
/// the language expected for the meeting; a recogniser may lean on it for
/// ambiguous text and returns nil when it has no opinion.
public protocol LanguageRecognizing: Sendable {
  func recognize(_ text: String, candidates: Set<LanguageTag>, hint: LanguageTag?) -> LanguageTag?
}

/// Fills `RawSegment.language` per segment. No engine can be forced per
/// segment, so the tag comes from the text: `NLLanguageRecognizer`
/// constrained to the candidates, with a fallback on function words where
/// NaturalLanguage is missing. Segments under `minimumWords` inherit the
/// previous segment's language (or the next tagged one at the start), because
/// four words are not enough to tell German from English reliably.
public struct LanguageTagger: Sendable {
  public static let defaultCandidates: Set<LanguageTag> = ["de", "en"]

  public var candidates: Set<LanguageTag>
  public var minimumWords: Int
  public var recognizer: any LanguageRecognizing

  public init(
    candidates: Set<LanguageTag> = LanguageTagger.defaultCandidates,
    minimumWords: Int = 4,
    recognizer: (any LanguageRecognizing)? = nil
  ) {
    self.candidates = candidates
    self.minimumWords = minimumWords
    self.recognizer = recognizer ?? Self.defaultRecognizer()
  }

  /// `NLLanguageRecognizer` where it exists, the function-word recogniser
  /// elsewhere.
  public static func defaultRecognizer() -> any LanguageRecognizing {
    #if canImport(NaturalLanguage)
      return NaturalLanguageRecognizer()
    #else
      return StopwordLanguageRecognizer()
    #endif
  }

  /// Tags every segment. `hint` is the meeting language when the pipeline
  /// knows it (the other lane's result, or the language WhisperKit was pinned
  /// to); it breaks ties and covers segments nothing else can tag.
  public func tag(_ segments: [RawSegment], hint: LanguageTag? = nil) -> [RawSegment] {
    var tagged = segments
    var decided: [LanguageTag?] = Array(repeating: nil, count: segments.count)
    for (index, segment) in segments.enumerated() where wordCount(segment.text) >= minimumWords {
      decided[index] = recognizer.recognize(segment.text, candidates: candidates, hint: hint)
    }
    let fallback = hint ?? dominant(decided, segments: segments)
    var previous: LanguageTag? = nil
    for index in segments.indices {
      if let language = decided[index] {
        previous = language
      } else {
        decided[index] = previous ?? nextDecided(decided, after: index) ?? fallback
      }
      tagged[index].language = decided[index]
    }
    return tagged
  }

  /// The language with the most seconds of tagged speech; nil when nothing
  /// is tagged.
  public func dominantLanguage(of segments: [RawSegment]) -> LanguageTag? {
    dominant(segments.map(\.language), segments: segments)
  }

  /// Adjacent segments with different languages; the bake-off's flip count.
  public static func languageFlips(in segments: [RawSegment]) -> Int {
    let languages = segments.compactMap(\.language)
    guard languages.count > 1 else { return 0 }
    return zip(languages, languages.dropFirst()).filter { $0 != $1 }.count
  }

  private func dominant(_ languages: [LanguageTag?], segments: [RawSegment]) -> LanguageTag? {
    var seconds: [LanguageTag: TimeInterval] = [:]
    for (language, segment) in zip(languages, segments) {
      guard let language else { continue }
      seconds[language, default: 0] += max(segment.duration, 0.001)
    }
    return seconds.max { lhs, rhs in
      if lhs.value != rhs.value { return lhs.value < rhs.value }
      return lhs.key.rawValue > rhs.key.rawValue
    }?.key
  }

  private func nextDecided(_ decided: [LanguageTag?], after index: Int) -> LanguageTag? {
    decided[(index + 1)...].first { $0 != nil } ?? nil
  }

  private func wordCount(_ text: String) -> Int {
    text.split { $0.isWhitespace }.count
  }
}

#if canImport(NaturalLanguage)
  /// `NLLanguageRecognizer` constrained to the candidates, with the hint
  /// weighted ahead of the others.
  public struct NaturalLanguageRecognizer: LanguageRecognizing {
    public init() {}

    public func recognize(_ text: String, candidates: Set<LanguageTag>, hint: LanguageTag?)
      -> LanguageTag?
    {
      let recognizer = NLLanguageRecognizer()
      let constraints = candidates.map { NLLanguage(rawValue: $0.rawValue) }
      recognizer.languageConstraints = constraints
      if let hint, candidates.contains(hint) {
        var hints: [NLLanguage: Double] = [:]
        for language in constraints {
          hints[language] =
            language.rawValue == hint.rawValue ? 0.6 : 0.4 / Double(candidates.count)
        }
        recognizer.languageHints = hints
      }
      recognizer.processString(text)
      guard let dominant = recognizer.dominantLanguage else {
        return StopwordLanguageRecognizer().recognize(text, candidates: candidates, hint: hint)
      }
      let tag = LanguageTag(rawValue: dominant.rawValue)
      return candidates.contains(tag) ? tag : nil
    }
  }
#endif

/// Counts German and English function words; a tie goes to the hint. The
/// deterministic recogniser for tests and the fallback where NaturalLanguage
/// is unavailable. Only knows `de` and `en`.
public struct StopwordLanguageRecognizer: LanguageRecognizing {
  static let german: Set<String> = [
    "der", "die", "das", "und", "ist", "nicht", "ich", "wir", "sie", "ein", "eine", "zu", "mit",
    "auf", "für", "von", "den", "dem", "des", "im", "ja", "nein", "auch", "noch", "schon", "wie",
    "aber", "oder", "wenn", "dann", "haben", "hat", "sind", "war", "es", "du", "er", "was", "dass",
    "bei", "nach", "über", "uns", "euch", "mal", "hier", "heute", "morgen", "machen", "können",
  ]
  static let english: Set<String> = [
    "the", "and", "is", "not", "i", "we", "you", "a", "an", "to", "with", "on", "for", "of", "in",
    "it", "that", "this", "are", "was", "have", "has", "be", "but", "or", "if", "then", "they",
    "he", "she", "what", "at", "by", "from", "about", "can", "will", "do", "let's", "today", "our",
  ]

  public init() {}

  public func recognize(_ text: String, candidates: Set<LanguageTag>, hint: LanguageTag?)
    -> LanguageTag?
  {
    let words = text.lowercased().split { !$0.isLetter && $0 != "'" }.map(String.init)
    let germanHits = words.filter(Self.german.contains).count
    let englishHits = words.filter(Self.english.contains).count
    let german: LanguageTag = "de"
    let english: LanguageTag = "en"
    if germanHits > englishHits, candidates.contains(german) { return german }
    if englishHits > germanHits, candidates.contains(english) { return english }
    if let hint, candidates.contains(hint) { return hint }
    return nil
  }
}
