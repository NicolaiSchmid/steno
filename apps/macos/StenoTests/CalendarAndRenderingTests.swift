import StenoCore
import XCTest

final class CalendarAndRenderingTests: XCTestCase {
  private let now = TestSupport.now

  private func event(_ id: String, from: TimeInterval, to: TimeInterval) -> CalendarEvent {
    CalendarEvent(
      id: id, title: id, start: now.addingTimeInterval(from), end: now.addingTimeInterval(to),
      attendees: [])
  }

  func testOverlappingEventWins() {
    let events = [event("later", from: 600, to: 1200), event("current", from: -300, to: 1800)]
    XCTAssertEqual(CalendarEvent.match(in: events, now: now)?.id, "current")
  }

  func testUpcomingWithinFifteenMinutes() {
    let events = [event("soon", from: 600, to: 1200), event("far", from: 3600, to: 7200)]
    XCTAssertEqual(CalendarEvent.match(in: events, now: now)?.id, "soon")
  }

  func testNothingWhenNoEventIsNear() {
    let events = [event("past", from: -7200, to: -3600), event("far", from: 3600, to: 7200)]
    XCTAssertNil(CalendarEvent.match(in: events, now: now))
    XCTAssertNil(CalendarEvent.match(in: [], now: now))
  }

  func testTranscriptTurnsGroupConsecutiveSegmentsOfOneSpeaker() {
    let segments = SampleData.segments()
    let turns = TranscriptTurns.group(segments)
    XCTAssertEqual(turns.count, 3, "two speakers plus an unassigned segment")
    var extra = segments[1]
    extra.id = UUID()
    extra.start = 5.6
    extra.end = 6
    extra.text = "Und Montag."
    let grouped = TranscriptTurns.group([segments[0], segments[1], extra])
    XCTAssertEqual(grouped.count, 2)
    XCTAssertEqual(grouped[1].text, "Ich prüfe das Budget bis Freitag. Und Montag.")
    XCTAssertEqual(grouped[1].start, 2.5)
  }

  /// The tab renders core's sections, so what it shows is exactly what
  /// `render` joins; no Markdown is parsed back.
  func testSummaryTabRendersCoresSectionsNotReparsedMarkdown() {
    let export = SampleData.export()
    let sections = SummaryMarkdown.sections(for: export)
    XCTAssertEqual(sections.map(\.heading), ["Executive Summary", "Offene Fragen"])
    XCTAssertTrue(
      sections[0].bullets.contains { $0.contains("**Nicolai**") },
      "the confirmed speaker's name is substituted before the tab sees it")
    XCTAssertEqual(
      sections.map(\.markdown).joined(separator: "\n\n") + "\n", SummaryMarkdown.render(export),
      "the sections are the render")
    let inline = MarkdownBlocks.inline("**Nicolai**: prüft")
    XCTAssertEqual(String(inline.characters), "Nicolai: prüft", "bold markers become styling")
  }

  func testClockTexts() {
    XCTAssertEqual(TimeInterval(65).clockText, "01:05")
    XCTAssertEqual(TimeInterval(3661).clockText, "1:01:01")
    XCTAssertEqual(TimeInterval(3661).timestampText, "01:01:01")
    XCTAssertEqual(TimeInterval(-3).clockText, "00:00")
  }
}
