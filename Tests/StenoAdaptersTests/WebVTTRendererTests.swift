import Foundation
import StenoCore
import Testing

@testable import StenoAdapters

@Suite struct WebVTTRendererTests {
  let renderer = ArtifactRenderer()
  let export = FixtureMeeting.export()

  @Test func matchesTheGolden() throws {
    try Snapshot.assert(renderer.renderVTT(export), matches: "snapshots/obsidian/transcript.vtt")
  }

  @Test func headerNoteOrderingAndVoices() {
    let vtt = renderer.renderVTT(export)
    #expect(
      vtt.hasPrefix(
        "WEBVTT - Steno 00000000-0000-0000-0000-000000000001\n\nNOTE\nProduktstrategie: \"90/10\" & Roadmap für Q4\n2026-09-24T12:00:00Z\n\n00:00:00.000 --> 00:00:04.200\n<v Nicolai Schmid>Guten Morgen zusammen, fangen wir mit der Roadmap an.\n\n00:00:04.500 --> 00:00:09.800\n<v Anna Müller>Gern. Ich habe die Zahlen für Q4 dabei.\n"
      ))
    let cues = vtt.components(separatedBy: " --> ")
    #expect(cues.count == 15, "fourteen cues")
    let starts = vtt.split(separator: "\n").filter { $0.contains(" --> ") }.map {
      String($0.prefix(12))
    }
    #expect(starts == starts.sorted())
    #expect(vtt.contains("\n<v Speaker 2>Ich kann das Protokoll verteilen.\n"))
    #expect(vtt.contains("\n<v Unknown>- Notiz: Einwurf aus dem Off.\n"))
    #expect(
      vtt.hasSuffix(
        "\n00:01:10.500 --> 00:01:12.000\n<v Nicolai Schmid>Danke, bis nächste Woche.\n"))
  }

  @Test func zeroLengthSegmentsGetAMillisecondAndPayloadsAreEscaped() {
    let vtt = renderer.renderVTT(export)
    #expect(vtt.contains("\n00:00:36.000 --> 00:00:36.001\n<v Speaker 2>Okay.\n"))
    #expect(
      vtt.contains("\n<v Nicolai Schmid># Punkt eins: Budget &lt;Kern&gt; &amp; Rest  offen\n"))
    #expect(
      !vtt.contains("-->\n<v Nicolai Schmid># Punkt eins: Budget <Kern>"),
      "the payload never carries a raw < or -->")
    #expect(WebVTTRenderer.payload("a\r\nb --> c") == "a b  c")
    #expect(WebVTTRenderer.annotation("A & B > C\nD") == "A  B  C D")
    #expect(WebVTTRenderer.noteText("x --> y") == "x  y")
    #expect(WebVTTRenderer.noteText("") == "Untitled")
  }

  @Test func aVoiceNameCannotCloseTheSpanOrCarryAnAmpersand() throws {
    var export = self.export
    export.persons[1].displayName = "Tom <Tommy> & Söhne > GmbH\nBerlin"
    export.meeting.title = "Q4 --> Q1 & <Plan>"
    let vtt = renderer.renderVTT(export)
    #expect(
      vtt.contains(
        "\n00:00:00.000 --> 00:00:04.200\n<v Tom <Tommy  Söhne  GmbH Berlin>Guten Morgen zusammen, fangen wir mit der Roadmap an.\n"
      ), "> and & are dropped from the annotation, < may stay")
    #expect(vtt.contains("\nNOTE\nQ4  Q1 & <Plan>\n"), "the NOTE block only loses -->")
    let voice = try Regex("^<v ([^>]*)>(.*)$")
    var cues = 0
    for line in vtt.split(separator: "\n") where line.hasPrefix("<v ") {
      let match = try #require(String(line).wholeMatch(of: voice), "\(line)")
      cues += 1
      #expect(!String(match.output[1].substring ?? "").contains("&"))
    }
    #expect(cues == 14, "every cue line still parses as one voice span")
    #expect(!vtt.contains("\n\n\n"), "no blank cue blocks")
  }
}
