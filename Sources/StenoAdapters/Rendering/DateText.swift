import Foundation

/// Calendar dates and wall-clock times as text, in a given time zone, from
/// the proleptic Gregorian calendar. No `DateFormatter`, so the bytes are the
/// same on every machine and never carry a locale's separators.
enum DateText {
  /// `"2026-09-24"`.
  static func day(_ date: Date, in timeZone: TimeZone) -> String {
    let c = components(date, timeZone)
    return "\(pad(c.year ?? 0, 4))-\(pad(c.month ?? 0))-\(pad(c.day ?? 0))"
  }

  /// `"14:00"`.
  static func clock(_ date: Date, in timeZone: TimeZone) -> String {
    let c = components(date, timeZone)
    return "\(pad(c.hour ?? 0)):\(pad(c.minute ?? 0))"
  }

  /// `"2026-09-24T14:00:00"`: Obsidian's Date & time property, no offset.
  static func dateTime(_ date: Date, in timeZone: TimeZone) -> String {
    let c = components(date, timeZone)
    return
      "\(day(date, in: timeZone))T\(pad(c.hour ?? 0)):\(pad(c.minute ?? 0)):\(pad(c.second ?? 0))"
  }

  /// `"2026-09-24T12:00:00Z"`: the same instant in UTC, for files that carry
  /// no time zone of their own.
  static func utc(_ date: Date) -> String {
    dateTime(date, in: TimeZone(identifier: "UTC")!) + "Z"
  }

  private static func components(_ date: Date, _ timeZone: TimeZone) -> DateComponents {
    var calendar = Calendar(identifier: .gregorian)
    calendar.timeZone = timeZone
    calendar.locale = Locale(identifier: "en_US_POSIX")
    return calendar.dateComponents([.year, .month, .day, .hour, .minute, .second], from: date)
  }

  private static func pad(_ value: Int, _ width: Int = 2) -> String {
    Timecode.pad(value, width: width)
  }
}
