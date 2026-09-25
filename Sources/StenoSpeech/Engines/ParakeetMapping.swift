import Foundation
import StenoCore

/// Everything `ParakeetEngine` does after `AsrManager` has returned: joins
/// the decoder tokens into words, the words into segments, and tags each
/// segment with a language. Framework-free, so the whole chain from token
/// timings to `RawSegment`s is tested without a model.
struct ParakeetMapping: Sendable {
  var aggregator = TokenAggregator()
  var segmenter = TranscriptSegmenter()
  var tagger = LanguageTagger()

  init(
    aggregator: TokenAggregator = TokenAggregator(),
    segmenter: TranscriptSegmenter = TranscriptSegmenter(),
    tagger: LanguageTagger = LanguageTagger()
  ) {
    self.aggregator = aggregator
    self.segmenter = segmenter
    self.tagger = tagger
  }

  /// `tokens` are the model's timed SentencePiece pieces; `text` its full
  /// transcript and `duration` the audio length, used for the one fallback
  /// segment when the model returned text but no token timings. `hint` is
  /// never sent to the model (Parakeet runs unpinned); it only steers the
  /// tagger.
  func segments(
    tokens: [TimedWord], text: String, duration: TimeInterval, hint: Locale.Language?
  ) -> [RawSegment] {
    var segments = segmenter.segments(from: aggregator.words(from: tokens))
    let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
    if segments.isEmpty, !trimmed.isEmpty {
      segments = [RawSegment(start: 0, end: max(0, duration), text: trimmed)]
    }
    return tagger.tag(segments, hint: hint.map(LanguageTag.init))
  }
}
