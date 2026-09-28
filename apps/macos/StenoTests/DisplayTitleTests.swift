import StenoCore
import XCTest

/// `Meeting.displayTitle`: the view-layer title over `titleOrigin`, with a
/// fixed `now`, calendar and locale, so the rule and not the machine decides
/// what the tests see. The stored `title` is never touched.
final class DisplayTitleTests: XCTestCase {
  private let locale = Locale(identifier: "en_US")
  /// 2026-09-24 09:00 UTC, a Thursday; 11:00 in Berlin, where it was recorded.
  private let startedAt = Date(timeIntervalSince1970: 1_790_240_400)
  private let oldDefault = "Call 2026-09-24 11:00"

  private func calendar(_ zone: String) -> Calendar {
    var calendar = Calendar(identifier: .gregorian)
    calendar.timeZone = TimeZone(identifier: zone)!
    calendar.locale = locale
    return calendar
  }

  private func meeting(_ origin: TitleOrigin, title: String? = nil) -> Meeting {
    var meeting = SampleData.meeting()
    meeting.title = title ?? oldDefault
    meeting.titleOrigin = origin
    meeting.startedAt = startedAt
    return meeting
  }

  func testDefaultTitleTodayReadsWeekdayAndTime() {
    let now = startedAt.addingTimeInterval(3 * 3_600)
    let title = meeting(.default).displayTitle(now: now, calendar: calendar("UTC"), locale: locale)
    XCTAssertTrue(title.hasPrefix("Thursday "), title)
    XCTAssertTrue(title.contains("9:00"), title)
    XCTAssertTrue(title.contains("AM"), title)
    XCTAssertFalse(title.contains("Call"), "the source is a chip, not a word in the title")
  }

  func testDefaultTitleSixDaysOldStillReadsTheWeekday() {
    let now = startedAt.addingTimeInterval(6 * 86_400 + 5 * 3_600)
    let title = meeting(.default).displayTitle(now: now, calendar: calendar("UTC"), locale: locale)
    XCTAssertTrue(title.hasPrefix("Thursday "), title)
  }

  func testDefaultTitleEightDaysOldReadsMonthDayAndTime() {
    let now = startedAt.addingTimeInterval(8 * 86_400)
    let title = meeting(.default).displayTitle(now: now, calendar: calendar("UTC"), locale: locale)
    XCTAssertTrue(title.hasPrefix("Sep 24 "), title)
    XCTAssertTrue(title.contains("9:00"), title)
  }

  func testCalendarTitleRendersUnchanged() {
    let title = meeting(.calendar, title: "Produktstrategie 90/10")
      .displayTitle(now: startedAt, calendar: calendar("UTC"), locale: locale)
    XCTAssertEqual(title, "Produktstrategie 90/10")
  }

  func testSummaryTitleRendersUnchanged() {
    let title = meeting(.summary, title: "Wochenplanung")
      .displayTitle(now: startedAt, calendar: calendar("UTC"), locale: locale)
    XCTAssertEqual(title, "Wochenplanung")
  }

  /// Recognition no longer depends on the zone: a Berlin recording viewed
  /// from UTC shows the UTC time, and its stored "11:00" is not matched
  /// against.
  func testBerlinRecordingViewedInUTCShowsTheUTCTime() {
    let title = meeting(.default).displayTitle(
      now: startedAt, calendar: calendar("UTC"), locale: locale)
    XCTAssertTrue(title.contains("9:00"), title)
    XCTAssertFalse(title.contains("11:00"), title)
    let berlin = meeting(.default).displayTitle(
      now: startedAt, calendar: calendar("Europe/Berlin"), locale: locale)
    XCTAssertTrue(berlin.contains("11:00"), berlin)
  }

  /// The user typed exactly the old default string: it is theirs and stays.
  func testUserTitleIdenticalToTheOldDefaultRendersVerbatim() {
    let title = meeting(.user).displayTitle(
      now: startedAt, calendar: calendar("Europe/Berlin"), locale: locale)
    XCTAssertEqual(title, oldDefault)
  }

  func testTwentyFourHourLocaleFollowsTheSystem() {
    var berlin = calendar("Europe/Berlin")
    let german = Locale(identifier: "de_DE")
    berlin.locale = german
    let title = meeting(.default).displayTitle(now: startedAt, calendar: berlin, locale: german)
    XCTAssertTrue(title.hasPrefix("Donnerstag "), title)
    XCTAssertTrue(title.contains("11:00"), title)
    XCTAssertFalse(title.contains("AM"), title)
  }

  func testStoredTitleIsNeverMutated() {
    let stored = meeting(.default)
    _ = stored.displayTitle(now: startedAt, calendar: calendar("UTC"), locale: locale)
    XCTAssertEqual(stored.title, oldDefault)
    XCTAssertTrue(stored.isTitleDerived)
    XCTAssertFalse(meeting(.user).isTitleDerived)
  }
}
