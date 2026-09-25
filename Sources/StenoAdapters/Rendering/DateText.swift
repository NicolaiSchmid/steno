import Foundation

/// Calendar dates and wall-clock times as text in a given time zone, through
/// `Date.ISO8601FormatStyle`: no locale enters, so the bytes are the same on
/// every machine.
enum DateText {
  /// `"2026-09-24"`.
  static func day(_ date: Date, in timeZone: TimeZone) -> String {
    date.formatted(Date.ISO8601FormatStyle(timeZone: timeZone).year().month().day())
  }

  /// `"14:00"`.
  static func clock(_ date: Date, in timeZone: TimeZone) -> String {
    let time = date.formatted(
      Date.ISO8601FormatStyle(timeZone: timeZone).time(includingFractionalSeconds: false))
    return String(time.dropLast(3))
  }

  /// `"2026-09-24T14:00:00"`: Obsidian's Date & time property, no offset.
  static func dateTime(_ date: Date, in timeZone: TimeZone) -> String {
    date.formatted(
      Date.ISO8601FormatStyle(timeZone: timeZone).year().month().day()
        .time(includingFractionalSeconds: false))
  }

  /// `"2026-09-24T12:00:00Z"`: the same instant in UTC, for files that carry
  /// no time zone of their own.
  static func utc(_ date: Date) -> String {
    date.formatted(.iso8601)
  }
}
