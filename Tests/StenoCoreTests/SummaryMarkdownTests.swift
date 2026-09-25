import Foundation
import Testing

@testable import StenoCore

@Suite struct SummaryMarkdownTests {
  @Test func rendersHeadingsBulletsAndCurrentNames() throws {
    let export = SampleData.export()
    let markdown = SummaryMarkdown.render(export)
    #expect(markdown.hasPrefix("## Executive Summary\n\n- **Fokus**: "))
    #expect(markdown.contains("**Nicolai** schlägt vor"))
    #expect(markdown.contains("**Jérôme** will die Zahlen"))
    #expect(
      markdown.contains("\n\n## Offene Fragen\n\n- **Zeitplan**: Start im Oktober oder November?\n")
    )
    #expect(!markdown.contains("Speaker 1"))
    #expect(markdown.hasSuffix("\n"))
    try Snapshot.assert(markdown, matches: "snapshots/summary/sample-export.md")
  }

  @Test func renamingASpeakerChangesTheRenderingWithoutARerun() throws {
    var export = SampleData.export()
    export.persons[1].displayName = "Nicolai Schmid"
    var speaker = export.speakers[1]
    speaker.assignment = .unknown
    export.speakers[1] = speaker
    let markdown = SummaryMarkdown.render(export)
    #expect(markdown.contains("**Nicolai Schmid** schlägt vor"))
    #expect(markdown.contains("Speaker 2 will die Zahlen"), "an unresolved speaker keeps its label")
    #expect(!markdown.contains("**Speaker 2**"))
    try Snapshot.assert(markdown, matches: "snapshots/summary/renamed-speaker.md")
  }

  @Test func leadsWithLabelsAreNotDoubleBolded() {
    var export = SampleData.export()
    export.meeting.summary = SummaryDocument(
      templateID: "daily-standup",
      sections: [
        SummarySection(
          id: "progress", heading: "Progress",
          bullets: [
            SummaryBullet(lead: "Speaker 1", text: "Shipped the thing with Speaker 2."),
            SummaryBullet(lead: "", text: "No lead here."),
          ]),
        SummarySection(id: "empty", heading: "Empty", bullets: []),
      ])
    let markdown = SummaryMarkdown.render(export)
    #expect(
      markdown
        == "## Progress\n\n- **Nicolai**: Shipped the thing with **Jérôme**.\n- No lead here.\n")
  }

  @Test func longerLabelsWinAndMissingSummaryRendersEmpty() {
    var export = SampleData.export()
    var ten = export.speakers[0]
    ten.id = SampleData.uuid(29)
    ten.clusterLabel = "Speaker 10"
    ten.assignment = .confirmed(personID: SampleData.personJeromeID)
    export.speakers.append(ten)
    export.meeting.summary = SummaryDocument(
      templateID: "default",
      sections: [
        SummarySection(
          id: "s", heading: "S",
          bullets: [SummaryBullet(lead: "x", text: "Speaker 10 and Speaker 1.")])
      ])
    #expect(SummaryMarkdown.render(export) == "## S\n\n- **x**: **Jérôme** and **Nicolai**.\n")
    export.meeting.summary = nil
    #expect(SummaryMarkdown.render(export) == "")
  }
}
