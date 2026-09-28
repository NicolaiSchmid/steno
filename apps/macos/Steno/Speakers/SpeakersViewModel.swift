import Foundation
import StenoCore

/// The speakers of one meeting as the header row, the popover and the
/// transcript pickers show them: one row per speaker from the current
/// export, the options a picker offers, and the one write, `select`.
/// `MeetingDetailViewModel` feeds it on every export tick
/// (`update(export:)`); it then loads the name suggestions, the recent
/// people and the full people list in one cancellable task. Writes go
/// through `MeetingStore.confirm(speakerID:person:)`, which merges,
/// recomputes the voice and keeps the clip; the UI re-renders from the
/// store observation, so nothing here is optimistic.
@MainActor
@Observable
final class SpeakersViewModel {
  struct Row: Identifiable, Equatable {
    var speaker: Speaker
    /// The confirmed or suggested person, when the export has them.
    var person: Person?
    /// What the speaker said in the clip range; empty for confirmed rows.
    var excerpt: String
    /// Whether the sample clip file exists.
    var canPlay: Bool

    var id: UUID { speaker.id }
    var isConfirmed: Bool { speaker.assignment.isConfirmed }
    /// The confirmed or suggested person's name, else the cluster label.
    var displayName: String { person?.displayName ?? speaker.clusterLabel }
  }

  private(set) var rows: [Row] = []
  private(set) var export: MeetingExport?
  private(set) var recent: [Person] = []
  private(set) var persons: [Person] = []
  private(set) var suggestions: [UUID: SpeakerNameSuggestion] = [:]
  private(set) var error: String?
  /// Called after every write that changed a speaker, so the owner can
  /// schedule the re-export.
  var onWrite: (@MainActor () -> Void)?
  let player = ClipPlayer()

  private let store: MeetingStore
  private let now: @Sendable () -> Date
  private var loadTask: Task<Void, Never>?

  init(store: MeetingStore, now: @escaping @Sendable () -> Date) {
    self.store = store
    self.now = now
  }

  convenience init(environment: AppEnvironment) {
    self.init(store: environment.store, now: environment.now)
  }

  var unconfirmedCount: Int { rows.filter { !$0.isConfirmed }.count }

  func row(_ id: UUID) -> Row? {
    rows.first { $0.id == id }
  }

  /// A new export from the store observation: rows are rebuilt at once, the
  /// people lists reload in the background (the previous load is dropped).
  func update(export: MeetingExport) {
    self.export = export
    rows = export.speakers.map { speaker in
      Row(
        speaker: speaker,
        person: speaker.personID.flatMap { id in export.persons.first { $0.id == id } },
        excerpt: speaker.assignment.isConfirmed
          ? "" : SpeakerExcerpts.text(for: speaker, in: export.segments),
        canPlay: speaker.sampleClipURL.map { FileManager.default.fileExists(atPath: $0.path) }
          ?? false)
    }
    loadTask?.cancel()
    let meetingID = export.meeting.id
    loadTask = Task { [weak self, store] in
      do {
        let suggestions = try await store.nameSuggestions(meetingID: meetingID)
        let recent = try await store.recentPersons()
        let persons = try await store.persons()
        guard !Task.isCancelled, let self else { return }
        self.suggestions = Dictionary(
          suggestions.map { ($0.speakerID, $0) }, uniquingKeysWith: { first, _ in first })
        self.recent = recent
        self.persons = persons
      } catch {
        guard !Task.isCancelled, let self else { return }
        self.error = "People could not be loaded: \(error)"
      }
    }
  }

  /// The suggested person's name for a `.suggested` speaker, what the field
  /// opens with; nil otherwise.
  func prefill(for speakerID: UUID) -> String? {
    guard let row = row(speakerID), case .suggested = row.speaker.assignment else { return nil }
    return row.person?.displayName
  }

  /// The ranked options for one speaker's picker (`SpeakerOptions.build`).
  /// Reads only.
  func options(for speakerID: UUID, query: String) -> [SpeakerOptions.Option] {
    guard let export, let speaker = export.speakers.first(where: { $0.id == speakerID }) else {
      return []
    }
    var known = persons
    for person in export.persons where !known.contains(where: { $0.id == person.id }) {
      known.append(person)
    }
    let suggestion = suggestions[speakerID].flatMap { $0.name?.isEmpty == false ? $0 : nil }
    return SpeakerOptions.build(
      speaker: speaker, speakers: export.speakers, persons: known,
      participants: export.participants, suggestion: suggestion, recent: recent, query: query)
  }

  /// The one write: a person option confirms that person (merging when they
  /// already own another speaker here, inside `confirm`); a create option
  /// resolves the name to an existing or new person first, taking the email
  /// of a calendar attendee of that name. Selecting the speaker's own person
  /// again changes nothing. A speaker the export does not list is ignored.
  func select(_ option: SpeakerOptions.Option, for speakerID: UUID) async {
    guard let row = row(speakerID) else { return }
    do {
      let person: Person
      switch option.kind {
      case .person(let known):
        person = known
      case .create(let name):
        let attendee = export?.participants.first {
          $0.role == .them && Person.namesMatch($0.displayName, name)
        }
        person = try await store.resolvePerson(named: name, email: attendee?.email, now: now())
      }
      if row.speaker.assignment == .confirmed(personID: person.id) { return }
      try await store.confirm(speakerID: speakerID, person: person)
      onWrite?()
    } catch {
      self.error = "Speaker could not be named: \(error)"
    }
  }

  // MARK: - Playback

  func play(_ id: UUID) {
    guard let row = row(id), let url = row.speaker.sampleClipURL else {
      error = "This speaker has no sample clip."
      return
    }
    if !player.play(url) {
      error = "The sample clip could not be played (\(url.lastPathComponent))."
    }
  }

  func stopPlayback() {
    player.stop()
  }

  var playing: UUID? {
    guard let url = player.playingURL else { return nil }
    return rows.first { $0.speaker.sampleClipURL == url }?.id
  }
}
