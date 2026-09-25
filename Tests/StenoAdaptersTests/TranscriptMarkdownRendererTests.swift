import Foundation
import StenoCore
import Testing

@testable import StenoAdapters

@Suite struct TranscriptMarkdownRendererTests {
  let renderer = ArtifactRenderer()
  let export = FixtureMeeting.export()

  @Test func matchesTheGoldens() throws {
    try Snapshot.assert(
      renderer.renderTranscript(export, options: FixtureMeeting.plain),
      matches: "snapshots/obsidian/transcript-plain.md")
    try Snapshot.assert(
      renderer.renderTranscript(export, options: FixtureMeeting.wikilink),
      matches: "snapshots/obsidian/transcript-wikilink.md")
  }

  @Test func groupsConsecutiveSegmentsIntoTurnsAndBreaksParagraphsAtGaps() {
    let note = renderer.renderTranscript(export, options: FixtureMeeting.wikilink)
    #expect(
      note.hasPrefix(
        "---\ntitle: \"Produktstrategie: \\\"90/10\\\" & Roadmap für Q4 — Transcript\"\ntype: \"transcript\"\nsteno_id: \"00000000-0000-0000-0000-000000000001\"\n---\n"
      ))
    #expect(
      note.contains(
        "\n## [[Nicolai Schmid]] — 00:00:00\n\nGuten Morgen zusammen, fangen wir mit der Roadmap an.\n"
      ))
    #expect(
      note.contains(
        "\n## [[Anna Müller]] — 00:00:04\n\nGern. Ich habe die Zahlen für Q4 dabei. Der Kern bekommt 90 Prozent, der Rest 10.\n\nDas ACME-Angebot muss bis zum 1. Oktober raus.\n"
      ), "segments 0.2 s apart join a paragraph, a 3.8 s gap starts a new one")
    #expect(
      note.contains("\n## Speaker 2 — 00:00:30\n\nIch kann das Protokoll verteilen. Okay.\n"),
      "an unresolved speaker is not linked")
    #expect(note.contains("\n## Unknown — 00:00:37\n\n\\- Notiz: Einwurf aus dem Off.\n"))
    #expect(note.contains("\n\\# Punkt eins: Budget <Kern> & Rest --> offen\n"))
    #expect(note.contains("\n\\1. Wir starten im Oktober.\n"))
    #expect(note.contains("\n\\> Zitat aus dem Kundenbrief.\n"))
    #expect(
      note.components(separatedBy: "\n## ").count - 1 == 11, "fourteen segments form eleven turns")
    #expect(note.hasSuffix("\n## [[Nicolai Schmid]] — 00:01:10\n\nDanke, bis nächste Woche.\n"))
  }

  @Test func plainStyleWritesNamesWithoutLinks() {
    let note = renderer.renderTranscript(export, options: FixtureMeeting.plain)
    #expect(!note.contains("[["))
    #expect(note.contains("\n## Anna Müller — 00:00:04\n"))
  }

  @Test func emptyTranscriptAndBlankSegments() {
    var export = self.export
    export.segments = []
    #expect(
      renderer.renderTranscript(export, options: .plain).hasSuffix(
        "— Transcript\n\nNo transcript.\n"))
    export.segments = [
      TranscriptSegment(
        id: SampleData.uuid(1), meetingID: export.meeting.id, start: 1, end: 2,
        speakerID: FixtureMeeting.speakerOneID, lane: .system, text: "  \n ", rawText: ""),
      TranscriptSegment(
        id: SampleData.uuid(2), meetingID: export.meeting.id, start: 2, end: 3,
        speakerID: FixtureMeeting.speakerOneID, lane: .system, text: "Line one\nline two",
        rawText: ""),
    ]
    let note = renderer.renderTranscript(export, options: .plain)
    #expect(
      note.hasSuffix("\n## Anna Müller — 00:00:02\n\nLine one line two\n"),
      "blank segments are skipped, line breaks become spaces")
  }
}
