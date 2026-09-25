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

  @Test func labelsAreReplacedOnlyAsWholeWords() {
    var export = SampleData.export()
    // The mic lane's speaker is labelled "Me" and confirmed once the user is known.
    var me = export.speakers[0]
    me.id = SampleData.uuid(28)
    me.clusterLabel = LaneMerger.meSpeakerLabel
    export.speakers.append(me)
    export.meeting.summary = SummaryDocument(
      templateID: "default",
      sections: [
        SummarySection(
          id: "s", heading: "S",
          bullets: [
            SummaryBullet(lead: "Meeting", text: "Melanie joined the Meeting with Me."),
            SummaryBullet(lead: "", text: "Me: Speaker 1's plan; Speaker 12 disagrees (Me)."),
          ])
      ])
    #expect(
      SummaryMarkdown.render(export) == """
        ## S

        - **Meeting**: Melanie joined the Meeting with **Nicolai**.
        - **Nicolai**: **Nicolai**'s plan; Speaker 12 disagrees (**Nicolai**).

        """)
  }

  @Test func sectionsCarryTheHeadingsAndSubstitutedBulletsThatRenderJoins() {
    var export = SampleData.export()
    export.meeting.summary?.sections.append(
      SummarySection(id: "empty", heading: "Leer", bullets: []))
    let sections = SummaryMarkdown.sections(for: export)
    #expect(
      sections == [
        RenderedSection(
          id: "executive-summary", heading: "Executive Summary",
          bullets: [
            "**Fokus**: **Nicolai** schlägt vor, 90 Prozent auf den Kern zu setzen.",
            "**Budget**: **Jérôme** will die Zahlen bis Freitag prüfen.",
          ]),
        RenderedSection(
          id: "open-questions", heading: "Offene Fragen",
          bullets: ["**Zeitplan**: Start im Oktober oder November?"]),
      ], "empty sections are skipped, as in the Markdown")
    #expect(
      sections[1].body == "- **Zeitplan**: Start im Oktober oder November?")
    #expect(
      sections[1].markdown == "## Offene Fragen\n\n- **Zeitplan**: Start im Oktober oder November?")
    #expect(
      SummaryMarkdown.render(export)
        == sections.map(\.markdown).joined(separator: "\n\n") + "\n",
      "render is the join over sections")
    #expect(SummaryMarkdown.render(export) == SummaryMarkdown.render(SampleData.export()))

    var unresolved = export
    unresolved.speakers[1].assignment = .unknown
    let bullets = SummaryMarkdown.sections(for: unresolved).flatMap(\.bullets)
    #expect(bullets.contains("**Budget**: Speaker 2 will die Zahlen bis Freitag prüfen."))
    #expect(!bullets.joined().contains("**Speaker 2**"))

    export.meeting.summary = nil
    #expect(SummaryMarkdown.sections(for: export).isEmpty)
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
