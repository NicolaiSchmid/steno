import Foundation
import Testing

@testable import StenoCore

@Suite struct SpeakerExcerptsTests {
  static let speakerID = SampleData.uuid(23)
  static let otherSpeakerID = SampleData.uuid(24)

  static func speaker(range: ClosedRange<TimeInterval>?) -> Speaker {
    Speaker(
      id: speakerID, meetingID: SampleData.meetingID, clusterLabel: "Speaker 3",
      sampleClipRange: range, clusterConfidence: 0.5)
  }

  static func segment(
    _ n: Int, _ start: TimeInterval, _ end: TimeInterval, _ text: String,
    speaker: UUID? = speakerID
  ) -> TranscriptSegment {
    TranscriptSegment(
      id: SampleData.uuid(100 + n), meetingID: SampleData.meetingID, start: start, end: end,
      speakerID: speaker, lane: .mic, text: text, rawText: text.lowercased())
  }

  @Test func joinsTheSegmentsOverlappingTheClipInStartOrder() {
    let segments = [
      Self.segment(2, 12, 14, " neunzig Prozent auf den Kern. "),
      Self.segment(1, 10, 12, "Wir setzen"),
      Self.segment(3, 20, 25, "Später reden wir über das Budget."),
      Self.segment(4, 10.5, 13, "Ja.", speaker: Self.otherSpeakerID),
    ]
    let text = SpeakerExcerpts.text(for: Self.speaker(range: 10...14), in: segments)
    #expect(text == "Wir setzen neunzig Prozent auf den Kern.")
  }

  @Test func aNilRangeUsesTheLongestSegment() {
    let segments = [
      Self.segment(1, 0, 1, "Kurz."),
      Self.segment(2, 5, 9, "Der längste Beitrag dieses Sprechers."),
      Self.segment(3, 20, 22, "Mittel."),
      Self.segment(4, 30, 60, "Jemand anders redet lange.", speaker: Self.otherSpeakerID),
    ]
    let text = SpeakerExcerpts.text(for: Self.speaker(range: nil), in: segments)
    #expect(text == "Der längste Beitrag dieses Sprechers.")
  }

  @Test func aRangeWithNoOverlapFallsBackToTheLongestSegment() {
    let segments = [
      Self.segment(1, 0, 1, "Kurz."),
      Self.segment(2, 5, 9, "Der längste Beitrag."),
    ]
    let text = SpeakerExcerpts.text(for: Self.speaker(range: 40...43), in: segments)
    #expect(text == "Der längste Beitrag.")
  }

  @Test func noSegmentsGiveAnEmptyString() {
    let segments = [Self.segment(1, 0, 4, "Nur der andere.", speaker: Self.otherSpeakerID)]
    #expect(SpeakerExcerpts.text(for: Self.speaker(range: 0...4), in: segments) == "")
    #expect(SpeakerExcerpts.text(for: Self.speaker(range: nil), in: []) == "")
  }

  @Test func aPartialOverlapGetsALeadingEllipsis() {
    let speaker = SampleData.speakers()[1]  // range 3...5.5, segment 2.5...5.5
    let text = SpeakerExcerpts.text(for: speaker, in: SampleData.segments())
    #expect(text == "…Ich prüfe das Budget bis Freitag.")

    let fullyInside = SampleData.speakers()[0]  // range 0.5...4.5, segment 0...2.5
    #expect(
      SpeakerExcerpts.text(for: fullyInside, in: SampleData.segments())
        == "…Wir setzen neunzig Prozent auf den Kern.")
  }

  @Test func aSegmentStartingInsideTheClipHasNoLeadingEllipsis() {
    let segments = [Self.segment(1, 3.5, 8, "Beginnt im Clip.")]
    let text = SpeakerExcerpts.text(for: Self.speaker(range: 3...5.5), in: segments)
    #expect(text == "Beginnt im Clip.")
  }

  @Test func longTextIsCutAtTheCapWithATrailingEllipsis() {
    let words = String(repeating: "Wort ", count: 60)  // 300 characters
    let segments = [Self.segment(1, 0, 10, words)]
    let text = SpeakerExcerpts.text(for: Self.speaker(range: 0...10), in: segments)
    #expect(text.hasSuffix("…"))
    #expect(text.count <= SpeakerExcerpts.maxLength + 1)
    #expect(
      text.dropLast()
        == words.trimmingCharacters(in: .whitespaces).prefix(SpeakerExcerpts.maxLength)
        .trimmingCharacters(in: .whitespaces))
    #expect(!text.hasSuffix(" …"))
  }

  @Test func textAtTheCapIsNotCut() {
    let exact = String(repeating: "a", count: SpeakerExcerpts.maxLength)
    let segments = [Self.segment(1, 0, 10, exact)]
    #expect(SpeakerExcerpts.text(for: Self.speaker(range: 0...10), in: segments) == exact)
  }
}
