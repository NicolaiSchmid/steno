import Foundation
import Testing

@testable import StenoCore

@Suite struct CalendarMatchTests {
  let now = Date(timeIntervalSince1970: 1_790_413_200)

  func event(_ id: String, _ startOffset: TimeInterval, _ endOffset: TimeInterval)
    -> (id: String, start: Date, end: Date)
  {
    (id, now.addingTimeInterval(startOffset), now.addingTimeInterval(endOffset))
  }

  @Test func overlappingEventWins() {
    let events = [event("later", 600, 1200), event("current", -600, 600)]
    #expect(CalendarMatch.pick(events: events, now: now) == "current")
  }

  @Test func mostRecentlyStartedOverlapWins() {
    let events = [event("long", -3600, 3600), event("short", -300, 300)]
    #expect(CalendarMatch.pick(events: events, now: now) == "short")
  }

  @Test func nextEventWithinLookahead() {
    let events = [event("far", 2000, 2600), event("soon", 500, 1100), event("past", -1200, -600)]
    #expect(CalendarMatch.pick(events: events, now: now) == "soon")
    #expect(CalendarMatch.pick(events: events, now: now, lookahead: 400) == nil)
    #expect(CalendarMatch.pick(events: events, now: now, lookahead: 2000) == "soon")
  }

  @Test func endedEventsNeverMatch() {
    #expect(CalendarMatch.pick(events: [event("past", -1200, 0)], now: now) == nil)
    #expect(CalendarMatch.pick(events: [], now: now) == nil)
  }

  @Test func tiesBreakOnID() {
    let events = [event("b", 0, 600), event("a", 0, 600)]
    #expect(CalendarMatch.pick(events: events, now: now) == "a")
  }
}
