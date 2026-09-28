import Foundation

/// The ranked option list behind the speaker picker: who a speaker could be,
/// best guess first, in one flat list. Pure and store-free; the app feeds it
/// the meeting's export, the LLM's name suggestion and
/// `MeetingStore.recentPersons()`.
///
/// Order: the voice match ("Sounds like"), the LLM guess ("Mentioned"),
/// unassigned calendar attendees ("Attendee"), persons owning another speaker
/// of this meeting ("In this meeting"), recent persons, then, once the query
/// is non-empty, every other person whose name contains it, and a Create row
/// last. Each person appears at most once and a confirmed speaker's own
/// person is never offered.
public enum SpeakerOptions {
  /// One row of the picker.
  public struct Option: Sendable, Equatable, Identifiable {
    public enum Kind: Sendable, Equatable {
      /// Choosing it confirms the speaker as this person.
      case person(Person)
      /// Choosing it resolves the name through `MeetingStore.resolvePerson`
      /// and confirms.
      case create(String)
    }

    /// The trailing label that says why the row is offered; nil for recent
    /// persons, search hits and the plain Create row.
    public enum Tag: String, Sendable {
      case soundsLike = "Sounds like"
      case mentioned = "Mentioned"
      case attendee = "Attendee"
      case inThisMeeting = "In this meeting"
    }

    public var kind: Kind
    public var tag: Tag?

    public init(kind: Kind, tag: Tag? = nil) {
      self.kind = kind
      self.tag = tag
    }

    /// The person's uuid string, or `"create:"` followed by the text.
    public var id: String {
      switch kind {
      case .person(let person): person.id.uuidString
      case .create(let text): "create:" + text
      }
    }

    /// The person's name, or the text a Create row would create.
    public var displayName: String {
      switch kind {
      case .person(let person): person.displayName
      case .create(let text): text
      }
    }
  }

  /// Builds the list for `speaker`.
  ///
  /// - `speakers`: every speaker of the meeting, including `speaker`.
  /// - `persons`: every known person (`MeetingStore.persons()`), the pool a
  ///   query searches.
  /// - `participants`: the meeting's participants; those with `role == .them`
  ///   are the calendar attendees. An attendee is offered as the person their
  ///   `personID` or name resolves to, else as a create row; attendees whose
  ///   person owns another speaker here appear under "In this meeting" instead.
  /// - `suggestion`: the LLM's name guess for this speaker, if any.
  /// - `recent`: `MeetingStore.recentPersons()`, already ordered.
  /// - `query`: the field text. Whitespace-only counts as empty. A non-empty
  ///   query filters every option by a case- and diacritic-insensitive
  ///   substring test on its display name, so "jerome" finds Jérôme, and adds
  ///   a Create row unless a listed option already has that exact name.
  public static func build(
    speaker: Speaker,
    speakers: [Speaker],
    persons: [Person],
    participants: [Participant],
    suggestion: SpeakerNameSuggestion?,
    recent: [Person],
    query: String
  ) -> [Option] {
    let query = query.trimmingCharacters(in: .whitespacesAndNewlines)
    var list = List(query: query, excludedPersonID: confirmedPersonID(of: speaker))
    let personsByID = Dictionary(persons.map { ($0.id, $0) }) { first, _ in first }
    let othersPersonIDs = Set(
      speakers.lazy.filter { $0.id != speaker.id }.compactMap(\.assignment.personID))

    if case .suggested(let personID, _) = speaker.assignment,
      let person = personsByID[personID]
    {
      list.add(.person(person), tag: .soundsLike)
    }

    if let name = suggestion?.name?.trimmingCharacters(in: .whitespacesAndNewlines),
      !name.isEmpty
    {
      if let person = persons.first(where: { Person.namesMatch($0.displayName, name) }) {
        list.add(.person(person), tag: .mentioned)
      } else {
        list.add(.create(name), tag: .mentioned)
      }
    }

    for participant in participants where participant.role == .them {
      if list.containsName(participant.displayName) { continue }
      let person =
        participant.personID.flatMap { personsByID[$0] }
        ?? persons.first { Person.namesMatch($0.displayName, participant.displayName) }
      if let person {
        if othersPersonIDs.contains(person.id) { continue }
        list.add(.person(person), tag: .attendee)
      } else {
        list.add(.create(participant.displayName), tag: .attendee)
      }
    }

    for other in speakers where other.id != speaker.id {
      if let personID = other.assignment.personID, let person = personsByID[personID] {
        list.add(.person(person), tag: .inThisMeeting)
      }
    }

    for person in recent {
      list.add(.person(person), tag: nil)
    }

    if !query.isEmpty {
      for person in persons where contains(person.displayName, query) {
        list.add(.person(person), tag: nil)
      }
      if !list.containsName(query) {
        list.options.append(Option(kind: .create(query)))
      }
    }

    return list.options
  }

  /// The person a `.confirmed` speaker already is. A `.suggested` person is
  /// an offer ("Sounds like"), not an exclusion.
  private static func confirmedPersonID(of speaker: Speaker) -> UUID? {
    if case .confirmed(let personID) = speaker.assignment { return personID }
    return nil
  }

  /// Case- and diacritic-insensitive substring test.
  static func contains(_ text: String, _ query: String) -> Bool {
    text.range(of: query, options: [.caseInsensitive, .diacriticInsensitive]) != nil
  }

  private struct List {
    var query: String
    var excludedPersonID: UUID?
    var options: [Option] = []
    private var listedPersonIDs: Set<UUID> = []

    init(query: String, excludedPersonID: UUID?) {
      self.query = query
      self.excludedPersonID = excludedPersonID
    }

    /// Appends the option unless the person is excluded or already listed, an
    /// option of the same name is already listed (create rows), or the query
    /// filters it out.
    mutating func add(_ kind: Option.Kind, tag: Option.Tag?) {
      let option = Option(kind: kind, tag: tag)
      switch kind {
      case .person(let person):
        if person.id == excludedPersonID || listedPersonIDs.contains(person.id) { return }
      case .create(let text):
        if containsName(text) { return }
      }
      if !query.isEmpty, !SpeakerOptions.contains(option.displayName, query) { return }
      if case .person(let person) = kind { listedPersonIDs.insert(person.id) }
      options.append(option)
    }

    /// Whether a listed option's display name equals `name` by
    /// `Person.namesMatch`.
    func containsName(_ name: String) -> Bool {
      options.contains { Person.namesMatch($0.displayName, name) }
    }
  }
}
