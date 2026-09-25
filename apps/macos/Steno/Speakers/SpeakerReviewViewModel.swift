import Foundation
import StenoCore

/// The speaker review sheet: every speaker whose assignment is `.unknown`
/// or `.suggested`, with its clip, cosine candidates from `SpeakerMemory`
/// and the calendar participants as name suggestions. Naming and assigning
/// go through `MeetingStore.confirm(speakerID:person:memory:)` (the one
/// operation that enrols and deletes the clip); cluster merge and person
/// merge are two distinct store operations; `finish()` re-exports once.
@MainActor
@Observable
final class SpeakerReviewViewModel {
  struct Card: Identifiable, Equatable {
    var speaker: Speaker
    var candidates: [SpeakerMatch]
    var id: UUID { speaker.id }
    var clipURL: URL? { speaker.sampleClipURL }
  }

  let meetingID: UUID
  private(set) var cards: [Card] = []
  private(set) var allSpeakers: [Speaker] = []
  private(set) var knownPeople: [Person] = []
  private(set) var attendees: [Participant]
  private(set) var error: String?
  private(set) var redeliveries = 0
  private(set) var finished = false
  private(set) var skipped: Set<UUID> = []
  let player = ClipPlayer()

  private let store: MeetingStore
  private let memory: any SpeakerMemory
  private let pipeline: () -> ProcessingPipeline
  private let now: @Sendable () -> Date

  init(
    export: MeetingExport, store: MeetingStore, memory: any SpeakerMemory,
    pipeline: @escaping () -> ProcessingPipeline, now: @escaping @Sendable () -> Date
  ) {
    self.meetingID = export.meeting.id
    self.store = store
    self.memory = memory
    self.pipeline = pipeline
    self.now = now
    self.attendees = export.participants.filter { $0.role == .them }
    self.allSpeakers = export.speakers
    self.knownPeople = export.persons
    self.cards = export.speakers
      .filter { !$0.assignment.isConfirmed }
      .map { Card(speaker: $0, candidates: []) }
  }

  convenience init(export: MeetingExport, environment: AppEnvironment) {
    self.init(
      export: export, store: environment.store, memory: environment.speakerMemory,
      pipeline: { environment.pipeline }, now: environment.now)
  }

  var unresolved: [Card] { cards.filter { !skipped.contains($0.id) } }
  var isDone: Bool { unresolved.isEmpty }

  /// Candidates for every card and the current people list.
  func load() async {
    do {
      knownPeople = try await store.persons()
      for index in cards.indices {
        guard let embedding = cards[index].speaker.embedding else { continue }
        cards[index].candidates = try await memory.candidates(for: embedding, limit: 5)
      }
    } catch {
      self.error = "Suggestions unavailable: \(error)"
    }
  }

  func person(id: UUID) -> Person? {
    knownPeople.first { $0.id == id }
  }

  /// The `.suggested` person of a card, when the store has it.
  func suggestion(for card: Card) -> (person: Person, similarity: Float)? {
    guard case .suggested(let personID, let similarity) = card.speaker.assignment,
      let person = person(id: personID)
    else { return nil }
    return (person, similarity)
  }

  func displayName(_ speaker: Speaker) -> String {
    if let personID = speaker.personID, let person = person(id: personID) {
      return person.displayName
    }
    return speaker.clusterLabel
  }

  // MARK: - Playback

  func play(_ id: UUID) {
    guard let card = cards.first(where: { $0.id == id }), let url = card.clipURL else {
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
    return cards.first { $0.clipURL == url }?.id
  }

  // MARK: - Naming and assignment

  /// A new person with this name (or the existing person of the same name)
  /// confirmed for the speaker.
  func name(_ id: UUID, _ name: String) async {
    let trimmed = name.trimmingCharacters(in: .whitespaces)
    guard !trimmed.isEmpty else { return }
    let person =
      knownPeople.first { $0.displayName.caseInsensitiveCompare(trimmed) == .orderedSame }
      ?? Person(id: UUID(), displayName: trimmed, sampleCount: 0, createdAt: now())
    await confirm(id, person: person)
  }

  /// A calendar attendee becomes (or reuses) a person and is confirmed.
  func assign(_ id: UUID, attendee: Participant) async {
    let person =
      attendee.personID.flatMap(person(id:))
      ?? knownPeople.first {
        $0.displayName.caseInsensitiveCompare(attendee.displayName) == .orderedSame
      }
      ?? Person(
        id: UUID(), displayName: attendee.displayName, email: attendee.email, sampleCount: 0,
        createdAt: now())
    await confirm(id, person: person)
  }

  func assign(_ id: UUID, person: Person) async {
    await confirm(id, person: person)
  }

  func acceptSuggestion(_ id: UUID) async {
    guard let card = cards.first(where: { $0.id == id }), let match = suggestion(for: card) else {
      return
    }
    await confirm(id, person: match.person)
  }

  private func confirm(_ id: UUID, person: Person) async {
    do {
      try await store.confirm(speakerID: id, person: person, memory: memory)
      await reload()
    } catch {
      self.error = "Speaker could not be confirmed: \(error)"
    }
  }

  /// Leaves the assignment as it is; the card disappears for this session.
  func skip(_ id: UUID) {
    skipped.insert(id)
  }

  // MARK: - Merges

  /// Two clusters of this meeting are one voice: segments move to `target`,
  /// embeddings average, `source` is deleted.
  func mergeSpeakers(_ source: UUID, into target: UUID) async {
    guard source != target else { return }
    do {
      try await store.mergeSpeakers(source, into: target, meetingID: meetingID)
      await reload()
    } catch {
      self.error = "Speakers could not be merged: \(error)"
    }
  }

  /// One person recorded twice, across meetings.
  func mergePersons(keep: UUID, remove: UUID) async {
    guard keep != remove else { return }
    do {
      try await store.mergePersons(keep: keep, remove: remove)
      await reload()
    } catch {
      self.error = "People could not be merged: \(error)"
    }
  }

  private func reload() async {
    do {
      allSpeakers = try await store.speakers(meetingID: meetingID)
      knownPeople = try await store.persons()
      let existing = Dictionary(uniqueKeysWithValues: cards.map { ($0.id, $0.candidates) })
      cards = allSpeakers
        .filter { !$0.assignment.isConfirmed }
        .map { Card(speaker: $0, candidates: existing[$0.id] ?? []) }
      skipped = skipped.intersection(cards.map(\.id))
    } catch {
      self.error = "Speakers could not be reloaded: \(error)"
    }
  }

  // MARK: - Finish

  /// Re-exports once so the vault picks up the names. Enrolment already
  /// happened in `confirm`.
  func finish() async {
    guard !finished else { return }
    finished = true
    player.stop()
    do {
      try await pipeline().redeliver(meetingID: meetingID)
      redeliveries += 1
    } catch {
      self.error = "Re-export failed: \(error)"
    }
  }
}
