import EventKit
import Foundation
import StenoCore

/// EventKit behind `CalendarProviding`: one shared `EKEventStore`, today's
/// events with their attendees. No matching logic lives here; the app hands
/// the events to StenoCore's `CalendarMatch.pick`.
@MainActor
final class CalendarService: CalendarProviding {
  private let eventStore: EKEventStore
  private let calendar: Calendar

  init(eventStore: EKEventStore, calendar: Calendar = .current) {
    self.eventStore = eventStore
    self.calendar = calendar
  }

  func events(on day: Date) async throws -> [CalendarEvent] {
    guard EKEventStore.authorizationStatus(for: .event) == .fullAccess else { return [] }
    let start = calendar.startOfDay(for: day)
    guard let end = calendar.date(byAdding: .day, value: 1, to: start) else { return [] }
    let predicate = eventStore.predicateForEvents(withStart: start, end: end, calendars: nil)
    return eventStore.events(matching: predicate)
      .filter { !$0.isAllDay }
      .compactMap { event -> CalendarEvent? in
        guard let id = event.eventIdentifier, let eventStart = event.startDate,
          let eventEnd = event.endDate
        else { return nil }
        return CalendarEvent(
          id: id,
          title: event.title ?? "",
          start: eventStart,
          end: eventEnd,
          attendees: (event.attendees ?? []).compactMap(Self.attendee))
      }
      .sorted { $0.start < $1.start }
  }

  private static func attendee(_ participant: EKParticipant) -> CalendarAttendee? {
    guard participant.participantType == .person else { return nil }
    var email: String?
    if participant.url.scheme?.lowercased() == "mailto" {
      email = String(participant.url.absoluteString.dropFirst("mailto:".count))
    }
    let name = participant.name?.trimmingCharacters(in: .whitespaces) ?? ""
    guard !name.isEmpty || email != nil else { return nil }
    return CalendarAttendee(
      name: name.isEmpty ? (email ?? "") : name,
      email: email,
      isCurrentUser: participant.isCurrentUser)
  }
}

extension CalendarEvent {
  /// The event StenoCore's pure matcher picks for a recording starting at
  /// `now`: overlapping, else the next within fifteen minutes.
  static func match(in events: [CalendarEvent], now: Date) -> CalendarEvent? {
    let picked = CalendarMatch.pick(
      events: events.map { (id: $0.id, start: $0.start, end: $0.end) }, now: now)
    return events.first { $0.id == picked }
  }
}
