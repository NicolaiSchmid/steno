import Foundation

/// Picks the calendar event a recording most likely belongs to. Pure; the app
/// feeds it today's EventKit events.
public enum CalendarMatch {
  /// An event overlapping `now` wins (the most recently started one when
  /// several overlap); otherwise the next event starting within `lookahead`
  /// seconds. Ties break on `id` so the choice is deterministic.
  public static func pick(
    events: [(id: String, start: Date, end: Date)],
    now: Date,
    lookahead: TimeInterval = 900
  ) -> String? {
    let overlapping =
      events
      .filter { $0.start <= now && now < $0.end }
      .sorted { lhs, rhs in
        if lhs.start != rhs.start { return lhs.start > rhs.start }
        return lhs.id < rhs.id
      }
    if let current = overlapping.first { return current.id }

    let upcoming =
      events
      .filter { $0.start > now && $0.start.timeIntervalSince(now) <= lookahead }
      .sorted { lhs, rhs in
        if lhs.start != rhs.start { return lhs.start < rhs.start }
        return lhs.id < rhs.id
      }
    return upcoming.first?.id
  }
}
