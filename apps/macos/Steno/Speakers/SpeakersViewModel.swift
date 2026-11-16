import Foundation
import StenoCore

/// The speakers of one meeting as the header row, the popover and the
/// transcript pickers show them: one row per speaker from the current
/// export, the options a picker offers, and the writes: naming (`select`),
/// No to a suggestion (`reject`), Remove name (`clear`) and Not a person
/// (`setDismissed`), plus "This paragraph" (`moveTurn`) and Separate again
/// (`separate`). `MeetingDetailViewModel` feeds it on every export tick
/// (`update(export:combined:)`); it then loads the name suggestions, the user's
/// Noes, the recent people and the full people list in one cancellable
/// task. Naming goes through `MeetingStore.confirm(speakerID:person:)`,
/// which combines, recomputes the voice and keeps the clip; the UI re-renders
/// from the store observation, so nothing here is optimistic. A failed
/// write throws, so the bridge replies with it. Rust: `SpeakersViewModel`
/// in `steno-host`'s `speakers.rs`.
@MainActor
@Observable
final class SpeakersViewModel {
  struct Row: Identifiable, Equatable {
    var speaker: Speaker
    /// The confirmed or suggested person, when the export has them.
    var person: Person?
    /// What the speaker says in or around the sample clip (`SpeakerExcerpts`).
    var excerpt: String
    /// Whether `excerpt` is exactly what the clip plays, a quote; else it is
    /// context (`SpeakerExcerpts.Excerpt.inClip`).
    var excerptInClip = false
    /// Whether the sample clip file exists.
    var canPlay: Bool

    var id: UUID { speaker.id }
    var isConfirmed: Bool { speaker.assignment.isConfirmed }
    /// The confirmed or suggested person's name, else the cluster label.
    var displayName: String { person?.displayName ?? speaker.clusterLabel }
    /// How strongly a `.suggested` voice matches its person.
    var suggestionStrength: VoiceStrength? {
      speaker.assignment.similarity.flatMap(VoiceStrength.of)
    }
  }

  /// A voice combined into one of the rows, which Separate again makes its
  /// own voice again.
  struct Combined: Identifiable, Equatable {
    var merged: MergedSpeaker
    /// What the voice says in its own clip (`SpeakerExcerpts.clipText`).
    var excerpt: String?
    /// Whether its clip file exists.
    var canPlay: Bool

    var id: UUID { merged.speaker.id }
  }

  /// Why a turn's audio does not play.
  enum PlayTurnError: Error, Equatable {
    /// The meeting's recording is not on disk (retention removed it, or it
    /// was never kept).
    case recordingGone
    /// None of the turn's segments is in the transcript.
    case noSuchSegment
    /// The recording exists but did not play.
    case unplayable
  }

  private(set) var rows: [Row] = []
  /// Every voice combined into another, by cluster label.
  private(set) var combined: [Combined] = []
  private(set) var export: MeetingExport?
  private(set) var recent: [Person] = []
  private(set) var persons: [Person] = []
  private(set) var suggestions: [UUID: SpeakerNameSuggestion] = [:]
  /// The persons the user said No to, per speaker.
  private(set) var rejections: [UUID: Set<UUID>] = [:]
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

  /// The voices still to name or check; a dismissed voice is not one.
  var unconfirmedCount: Int { rows.filter { !$0.isConfirmed && !$0.speaker.dismissed }.count }

  func row(_ id: UUID) -> Row? {
    rows.first { $0.id == id }
  }

  /// The voices combined into the row `id`, by cluster label.
  func combinedVoices(into id: UUID) -> [Combined] {
    combined.filter { $0.merged.root == id }
  }

  /// A new export from the store observation, with the meeting's combined
  /// voices read beside it: rows are rebuilt at once, the suggestions, the
  /// user's Noes and the people lists reload in the background (the
  /// previous load is dropped). A voice match below `VoiceStrength.floor`
  /// reads as unknown everywhere.
  func update(export: MeetingExport, combined merged: [MergedSpeaker] = []) {
    var export = export
    export.speakers = export.speakers.map(Self.shown)
    self.export = export
    rows = export.speakers.map { speaker in
      let excerpt = SpeakerExcerpts.excerpt(for: speaker, in: export.segments)
      return Row(
        speaker: speaker,
        person: speaker.personID.flatMap { id in export.persons.first { $0.id == id } },
        excerpt: excerpt.text,
        excerptInClip: excerpt.inClip,
        canPlay: speaker.sampleClipURL.map { FileManager.default.fileExists(atPath: $0.path) }
          ?? false)
    }
    combined = merged.map { merged in
      Combined(
        merged: merged,
        excerpt: SpeakerExcerpts.clipText(for: merged.speaker, in: export.segments),
        canPlay: merged.speaker.sampleClipURL.map {
          FileManager.default.fileExists(atPath: $0.path)
        } ?? false)
    }
    loadTask?.cancel()
    let meetingID = export.meeting.id
    loadTask = Task { [weak self, store] in
      do {
        let suggestions = try await store.nameSuggestions(meetingID: meetingID)
        let rejections = try await store.suggestionRejections(meetingID: meetingID)
        let recent = try await store.recentPersons()
        let persons = try await store.persons()
        guard !Task.isCancelled, let self else { return }
        self.suggestions = Dictionary(
          suggestions.map { ($0.speakerID, $0) }, uniquingKeysWith: { first, _ in first })
        self.rejections = rejections
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
      participants: export.participants, suggestion: suggestion, recent: recent,
      rejected: rejections[speakerID] ?? [], query: query)
  }

  /// A voice match below `VoiceStrength.floor` is not shown: the speaker
  /// reads as unknown.
  private static func shown(_ speaker: Speaker) -> Speaker {
    guard let similarity = speaker.assignment.similarity, VoiceStrength.of(similarity) == nil
    else { return speaker }
    var speaker = speaker
    speaker.assignment = .unknown
    return speaker
  }

  /// Why `select` named nobody besides a store error.
  enum SelectError: Error, Equatable, CustomStringConvertible {
    /// The person already owns `voice` in this meeting, and confirming would
    /// merge the two speakers; the pick did not ask to combine.
    case wouldCombine(person: String, voice: String)

    var description: String {
      switch self {
      case .wouldCombine(let person, let voice):
        "\(person) already speaks as \(voice) in this meeting."
      }
    }
  }

  /// The one write: a person option confirms that person; a create option
  /// resolves the name to an existing or new person first, taking the email
  /// of a calendar attendee of that name. A person who owns another speaker
  /// here is combined into (inside `confirm`, undone by `separate`) only
  /// with `combine`, else
  /// `SelectError.wouldCombine` is thrown. Selecting the speaker's own person
  /// again changes nothing. A speaker the export does not list is ignored.
  /// Store errors are thrown, for the bridge to reply with.
  func select(_ option: SpeakerOptions.Option, for speakerID: UUID, combine: Bool = false)
    async throws
  {
    guard let row = row(speakerID) else { return }
    let person = try await person(for: option)
    if row.speaker.assignment == .confirmed(personID: person.id) { return }
    if !combine,
      let owner = try await combinedVoice(of: row.speaker, personID: person.id)
    {
      throw SelectError.wouldCombine(person: person.displayName, voice: owner.clusterLabel)
    }
    try await store.confirm(speakerID: speakerID, person: person)
    onWrite?()
  }

  /// The person an option stands for: a person option's person, or a create
  /// option's name resolved to an existing or new person, taking the email
  /// of a calendar attendee of that name. Nothing is saved.
  private func person(for option: SpeakerOptions.Option) async throws -> Person {
    switch option.kind {
    case .person(let known):
      return known
    case .create(let name):
      let attendee = export?.participants.first {
        $0.role == .them && Person.namesMatch($0.displayName, name)
      }
      return try await store.resolvePerson(named: name, email: attendee?.email, now: now())
    }
  }

  /// "This paragraph": the turn's segments move to the option's person in
  /// this meeting (`MeetingStore.moveTurn`), against the transcript of
  /// `generation`; `MeetingStoreError.staleTranscript` is thrown when it
  /// changed since. Nothing happens without an export.
  func moveTurn(
    segments: [TurnSegmentMove], generation: Int, to option: SpeakerOptions.Option
  ) async throws {
    guard let meetingID = export?.meeting.id else { return }
    let person = try await person(for: option)
    try await store.moveTurn(
      meetingID: meetingID, generation: generation, segments: segments, to: person)
    onWrite?()
  }

  /// Undo after a combine, and Separate again
  /// (`MeetingStore.separateSpeaker`): the combined voice is its own voice
  /// again. The store's refusals (`notCombined`, `staleTranscript`) are
  /// thrown.
  func separate(speakerID: UUID) async throws {
    try await store.separateSpeaker(speakerID)
    onWrite?()
  }

  /// No to the suggestion that the speaker is `personID`
  /// (`MeetingStore.rejectSuggestion`): that person is not suggested for
  /// this voice again and nobody's voice changes. A speaker the export does
  /// not list is ignored.
  func reject(speakerID: UUID, personID: UUID) async throws {
    guard row(speakerID) != nil else { return }
    try await store.rejectSuggestion(speakerID: speakerID, personID: personID)
    onWrite?()
  }

  /// Remove name (`MeetingStore.clear(speakerID:)`): the voice is unnamed
  /// again in this meeting; for "me" the meeting's "me" participant too. An
  /// unlisted or unnamed speaker is ignored.
  func clear(speakerID: UUID) async throws {
    guard let row = row(speakerID), row.speaker.assignment != .unknown else { return }
    try await store.clear(speakerID: speakerID)
    onWrite?()
  }

  /// Not a person, or a person again (`MeetingStore.setDismissed`). An
  /// unlisted speaker, or one already in that state, is ignored.
  func setDismissed(speakerID: UUID, _ dismissed: Bool) async throws {
    guard let row = row(speakerID), row.speaker.dismissed != dismissed else { return }
    try await store.setDismissed(speakerID: speakerID, dismissed)
    onWrite?()
  }

  /// The speaker `confirm` would merge `speaker` into when confirmed as
  /// `personID`: another speaker of the meeting confirmed as that person,
  /// the first by cluster label.
  private func combinedVoice(of speaker: Speaker, personID: UUID) async throws -> Speaker? {
    try await store.speakers(meetingID: speaker.meetingID)
      .filter { $0.id != speaker.id && $0.assignment == .confirmed(personID: personID) }
      .min { ($0.clusterLabel, $0.id.uuidString) < ($1.clusterLabel, $1.id.uuidString) }
  }

  // MARK: - Playback

  /// Plays the sample clip of a row or of a voice combined into one.
  func play(_ id: UUID) {
    let speaker = row(id)?.speaker ?? combined.first { $0.id == id }?.merged.speaker
    guard let url = speaker?.sampleClipURL else {
      error = "This speaker has no sample clip."
      return
    }
    if !player.play(url) {
      error = "The sample clip could not be played (\(url.lastPathComponent))."
    }
  }

  /// Plays one turn's own audio from the meeting's recording: from the
  /// earliest start to the latest end among `segmentIDs` the transcript
  /// has, at most `limit` seconds. The turn reads as playing (`playingTurn`)
  /// under the first id given, until it ends or anything else plays.
  func playTurn(segmentIDs: [UUID], limit: TimeInterval) throws {
    guard let export, let recording = export.audio?.url,
      FileManager.default.fileExists(atPath: recording.path)
    else { throw PlayTurnError.recordingGone }
    let listed = Set(segmentIDs)
    let segments = export.segments.filter { listed.contains($0.id) }
    guard let first = segmentIDs.first, let start = segments.map(\.start).min(),
      let end = segments.map(\.end).max()
    else { throw PlayTurnError.noSuchSegment }
    guard player.play(recording, from: start, until: min(end, start + limit), turn: first) else {
      throw PlayTurnError.unplayable
    }
  }

  func stopPlayback() {
    player.stop()
  }

  /// The row or combined voice whose clip plays; nil while a turn plays.
  var playing: UUID? {
    guard player.playingTurn == nil, let url = player.playingURL else { return nil }
    return rows.first { $0.speaker.sampleClipURL == url }?.id
      ?? combined.first { $0.merged.speaker.sampleClipURL == url }?.id
  }

  /// The turn whose audio plays, by the first segment id it was played with.
  var playingTurn: UUID? { player.playingTurn }
  /// The turn whose audio failed after it started (`ClipPlayer.failedTurn`).
  var failedTurn: UUID? { player.failedTurn }
}
