import Foundation
import StenoCore

/// Names the cleanup pass must spell exactly: the meeting's participants
/// (calendar attendees included, they are `Participant` rows) and every
/// known person. No product glossary in v1.
public struct Glossary: Sendable, Equatable {
  public var people: [String]

  public init(people: [String]) {
    self.people = people
  }

  /// Participants first, then known people, each name once
  /// (case-insensitively), in order of appearance.
  public init(input: CleanupInput) {
    var seen: Set<String> = []
    var people: [String] = []
    for name in input.participants.map(\.displayName) + input.knownPeople.map(\.displayName) {
      let trimmed = name.trimmingCharacters(in: .whitespaces)
      guard !trimmed.isEmpty, seen.insert(trimmed.lowercased()).inserted else { continue }
      people.append(trimmed)
    }
    self.people = people
  }
}
