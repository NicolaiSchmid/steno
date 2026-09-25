import Foundation
import StenoCore

/// The scope's fixed meeting folder: `Meetings/<yyyy-MM-dd>-<slug>`, the
/// date taken from `startedAt` in the given time zone. The folder's basename
/// is also the slug every note inside it is named after.
public enum MeetingFolder {
  public static let root = "Meetings"

  /// `"2026-09-24-produktstrategie-90-10-roadmap-fuer-q4"`.
  public static func basename(for meeting: Meeting, timeZone: TimeZone) -> String {
    "\(DateText.day(meeting.startedAt, in: timeZone))-\(Slug.title(meeting.title))"
  }

  /// `"Meetings/2026-09-24-produktstrategie-90-10-roadmap-fuer-q4"`.
  public static func path(for meeting: Meeting, timeZone: TimeZone) -> String {
    "\(root)/\(basename(for: meeting, timeZone: timeZone))"
  }
}
