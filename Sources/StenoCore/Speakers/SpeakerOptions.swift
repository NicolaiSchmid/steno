import Foundation

/// The ranked option list behind the speaker picker: who a speaker could be,
/// best guess first, in one flat list. Pure and store-free; the app feeds it
/// the meeting's export, the LLM's name suggestion, the user's Noes and
/// `MeetingStore.recentPersons()`. Rust: `build_options` in `steno-host`'s
/// `speakers.rs`, which this mirrors rule for rule.
///
/// Order: the voice match (`.voice`, with its strength, from
/// `VoiceStrength.floor` up), the LLM guess (`.mentioned`), invitees nobody
/// is named as (`.invited`, or `.lastInvitee` when this is the meeting's only
/// open voice and they are the only invitee left), persons owning another
/// speaker of this meeting (`.inThisMeeting`), recent persons, then, once
/// the query is non-empty, every other person whose name contains it, and a
/// Create row last. Each person appears at most once, a confirmed speaker's
/// own person is never offered, and a person the user said No to for this
/// voice only when typed.
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

    /// Why the row is offered; nil for recent persons, search hits and the
    /// plain Create row. Only `.voice` is voice evidence, and only it
    /// carries a strength.
    public enum Tag: Sendable, Equatable {
      /// The voice matches this person's voice memory.
      case voice(VoiceStrength)
      /// The summary heard the name for this speaker.
      case mentioned
      /// A calendar invitee nobody's voice is named as.
      case invited
      /// The only invitee left for the only voice left unnamed.
      case lastInvitee
      /// The person owns another voice of this meeting.
      case inThisMeeting
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
  ///   are the calendar invitees. An invitee is offered as the person their
  ///   `personID` or name resolves to, else as a create row; invitees whose
  ///   person owns another speaker here appear under `.inThisMeeting`
  ///   instead, and invitees the user said No to for this voice not at all.
  /// - `suggestion`: the LLM's name guess for this speaker, if any.
  /// - `recent`: `MeetingStore.recentPersons()`, already ordered.
  /// - `rejected`: the persons the user said No to for this speaker
  ///   (`MeetingStore.suggestionRejections(meetingID:)`). Neither they nor a
  ///   Create row of their name is listed while the query is empty; typed,
  ///   they are found.
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
    rejected: Set<UUID> = [],
    query: String
  ) -> [Option] {
    let query = query.trimmingCharacters(in: .whitespacesAndNewlines)
    var list = List(
      query: query, excludedPersonID: confirmedPersonID(of: speaker),
      rejected: persons.filter { rejected.contains($0.id) })
    let personsByID = Dictionary(persons.map { ($0.id, $0) }) { first, _ in first }
    let othersPersonIDs = Set(
      speakers.lazy.filter { $0.id != speaker.id }.compactMap(\.assignment.personID))

    if case .suggested(let personID, let similarity) = speaker.assignment,
      let strength = VoiceStrength.of(similarity),
      let person = personsByID[personID]
    {
      list.add(.person(person), tag: .voice(strength))
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

    let invitees = invitees(
      participants: participants, persons: persons, personsByID: personsByID,
      othersPersonIDs: othersPersonIDs, rejected: rejected)
    let open = openVoices(speakers)
    let lastVoice = open.count == 1 && open.first?.id == speaker.id
    let invited: Option.Tag = lastVoice && invitees.count == 1 ? .lastInvitee : .invited
    for invitee in invitees {
      let name =
        switch invitee {
        case .person(let person): person.displayName
        case .create(let name): name
        }
      if !list.containsName(name) { list.add(invitee, tag: invited) }
    }

    for other in speakers where other.id != speaker.id && !other.dismissed {
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

  /// The meeting's voices still to name: neither confirmed nor dismissed,
  /// and not "me", which no invitee is.
  private static func openVoices(_ speakers: [Speaker]) -> [Speaker] {
    speakers.filter {
      !$0.dismissed && !$0.assignment.isConfirmed
        && $0.id != LaneMerger.meSpeakerID(meetingID: $0.meetingID)
    }
  }

  /// The meeting's invitees nobody's voice is named or suggested as here,
  /// and not answered No for this voice, as a person when Steno knows them.
  private static func invitees(
    participants: [Participant], persons: [Person], personsByID: [UUID: Person],
    othersPersonIDs: Set<UUID>, rejected: Set<UUID>
  ) -> [Option.Kind] {
    participants.filter { $0.role == .them }.compactMap { participant in
      let name = participant.displayName.trimmingCharacters(in: .whitespacesAndNewlines)
      guard !name.isEmpty else { return nil }
      let person =
        participant.personID.flatMap { personsByID[$0] }
        ?? persons.first { Person.namesMatch($0.displayName, name) }
      guard let person else { return .create(name) }
      if othersPersonIDs.contains(person.id) || rejected.contains(person.id) { return nil }
      return .person(person)
    }
  }

  /// The person a `.confirmed` speaker already is. A `.suggested` person is
  /// an offer (the voice match), not an exclusion.
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
    /// The persons the user said No to for this voice: never suggested
    /// again, offered only when typed.
    var rejected: [Person]
    var options: [Option] = []
    private var listedPersonIDs: Set<UUID> = []

    init(query: String, excludedPersonID: UUID?, rejected: [Person]) {
      self.query = query
      self.excludedPersonID = excludedPersonID
      self.rejected = rejected
    }

    /// Appends the option unless the person is excluded or already listed,
    /// an option of the same name is already listed (create rows), the user
    /// said No to them for this voice and nothing is typed, or the query
    /// filters it out.
    mutating func add(_ kind: Option.Kind, tag: Option.Tag?) {
      let option = Option(kind: kind, tag: tag)
      switch kind {
      case .person(let person):
        if person.id == excludedPersonID || listedPersonIDs.contains(person.id)
          || (query.isEmpty && isRejected(person.displayName))
        {
          return
        }
      case .create(let text):
        if containsName(text) || (query.isEmpty && isRejected(text)) { return }
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

    private func isRejected(_ name: String) -> Bool {
      rejected.contains { Person.namesMatch($0.displayName, name) }
    }
  }
}
