import Foundation

/// What a finished capture hands to `LocalRecordingIntake.complete`: the
/// asset the writer produced (master, lanes, sidecars) and the seconds of
/// audio in it. StenoAudio's `CaptureResult` maps onto it; core does not
/// know the capture types.
public struct RecordingResult: Sendable, Equatable, Hashable {
  public var asset: AudioAsset
  public var duration: TimeInterval

  public init(asset: AudioAsset, duration: TimeInterval) {
    self.asset = asset
    self.duration = duration
  }
}

/// Errors of the Mac recording transaction beyond the store's own.
public enum LocalRecordingIntakeError: Error, Sendable, Equatable, CustomStringConvertible {
  /// `complete` on a meeting that is not `.recording`: nothing is written
  /// and nothing is marked failed.
  case notRecording(UUID, MeetingState.Kind)

  public var description: String {
    switch self {
    case .notRecording(let id, let state): "meeting \(id) is \(state.rawValue), not recording"
    }
  }
}

/// The Mac recording transaction, the sibling of `RecordingIntake` (phone).
/// `begin` writes the `.recording` row the list shows while the capture
/// session runs, with the calendar title and attendees; `complete` turns
/// the finished capture into a `.queued` meeting with its asset and hands
/// both to the pipeline; `fail` records why a recording never made it. The
/// caller keeps the capture session, the calendar lookup and the messages.
/// Every rule about the rows lives here (default title, attendee dedupe,
/// retention from `Settings` at completion time, `expiresAt` cleared, the
/// duration written) so the app, `steno record` and any later surface share
/// one spelling. Launch reconciliation is
/// `MeetingStore.failInterruptedRecordings(now:)` for `.recording` rows and
/// `ProcessingPipeline.resumeUnfinished()` for `.queued` and `.processing`.
public struct LocalRecordingIntake: Sendable {
  /// A calendar attendee other than the user; becomes a `.them`
  /// `Participant`.
  public struct Attendee: Sendable, Equatable, Hashable {
    public var displayName: String
    public var email: String?

    public init(displayName: String, email: String? = nil) {
      self.displayName = displayName
      self.email = email
    }
  }

  public let store: MeetingStore
  public let settings: SettingsStore
  public let enqueue: RecordingIntake.Enqueue
  public let now: @Sendable () -> Date

  /// `enqueue` is `ProcessingPipeline.enqueue(_:asset:)` in the app and the
  /// CLI; tests pass a counting closure.
  public init(
    store: MeetingStore,
    settings: SettingsStore,
    enqueue: @escaping RecordingIntake.Enqueue,
    now: @escaping @Sendable () -> Date = Date.init
  ) {
    self.store = store
    self.settings = settings
    self.enqueue = enqueue
    self.now = now
  }

  /// The production wiring: `enqueue` is `ProcessingPipeline.enqueue`.
  public init(
    store: MeetingStore,
    settings: SettingsStore,
    pipeline: ProcessingPipeline,
    now: @escaping @Sendable () -> Date = Date.init
  ) {
    self.init(
      store: store, settings: settings,
      enqueue: { meeting, asset in try await pipeline.enqueue(meeting, asset: asset) },
      now: now)
  }

  /// Writes the `.recording` meeting and its `.them` participants in one
  /// transaction and returns the meeting. An empty or nil `title` becomes
  /// "Call 2026-09-24 11:00" or "Meeting 2026-09-24 11:00" by `source`. The
  /// template is `Settings.defaultTemplateID`. Attendees are deduplicated by
  /// email, else by name, case-insensitively; blank names are dropped.
  public func begin(
    source: MeetingSource,
    title: String? = nil,
    calendarEventID: String? = nil,
    attendees: [Attendee] = [],
    startedAt: Date
  ) async throws -> Meeting {
    let settings = try await settings.load()
    let timestamp = now()
    let trimmedTitle = title?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
    let meeting = Meeting(
      id: UUID(),
      title: trimmedTitle.isEmpty
        ? Self.defaultTitle(source: source, startedAt: startedAt) : trimmedTitle,
      startedAt: startedAt,
      duration: 0,
      source: source,
      calendarEventID: calendarEventID,
      state: .recording,
      templateID: settings.defaultTemplateID,
      createdAt: timestamp,
      updatedAt: timestamp
    )
    let participants = Self.participants(from: attendees, meetingID: meeting.id)
    try await store.save(meeting, participants: participants)
    return meeting
  }

  /// Writes the duration, sets the asset's retention (`retention`, else
  /// `Settings.defaultRetention` as it is now, so a change made during the
  /// recording applies) with `expiresAt` cleared, and enqueues the meeting
  /// as `.queued`. Returns the meeting as handed to the pipeline. A meeting
  /// that is not `.recording` throws `LocalRecordingIntakeError.notRecording`
  /// and is left alone; any other failure (the enqueue among them) marks the
  /// meeting `.failed` with the reason and is rethrown.
  @discardableResult
  public func complete(
    meetingID: UUID, result: RecordingResult, retention: AudioRetention? = nil
  ) async throws -> Meeting {
    let timestamp = now()
    do {
      let retention =
        if let retention { retention } else { try await settings.load().defaultRetention }
      var meeting = try await store.update(meetingID: meetingID, now: timestamp) { meeting in
        guard meeting.state == .recording else {
          throw LocalRecordingIntakeError.notRecording(meetingID, meeting.state.kind)
        }
        meeting.duration = result.duration
      }
      meeting.state = .queued
      var asset = result.asset
      asset.meetingID = meetingID
      asset.retention = retention
      asset.expiresAt = nil
      try await enqueue(meeting, asset)
      return meeting
    } catch let error as LocalRecordingIntakeError {
      throw error
    } catch {
      try? await fail(meetingID: meetingID, reason: "Recording could not be saved: \(error)")
      throw error
    }
  }

  /// Marks the meeting `.failed(reason)`: a capture that could not start or
  /// could not be saved.
  public func fail(meetingID: UUID, reason: String) async throws {
    try await store.setState(.failed(reason: reason), meetingID: meetingID, now: now())
  }

  /// "Call 2026-09-24 11:00", "Meeting 2026-09-24 11:00" or "Phone recording
  /// 2026-09-24 11:00" in `timeZone`.
  public static func defaultTitle(
    source: MeetingSource, startedAt: Date, timeZone: TimeZone = .current
  ) -> String {
    let formatter = DateFormatter()
    formatter.locale = Locale(identifier: "en_US_POSIX")
    formatter.timeZone = timeZone
    formatter.dateFormat = "yyyy-MM-dd HH:mm"
    let kind =
      switch source {
      case .macCall: "Call"
      case .macInPerson: "Meeting"
      case .phone: "Phone recording"
      }
    return "\(kind) \(formatter.string(from: startedAt))"
  }

  /// One `.them` participant per distinct attendee, ids derived from the
  /// meeting id so a retried `begin` for the same meeting writes the same
  /// rows.
  static func participants(from attendees: [Attendee], meetingID: UUID) -> [Participant] {
    var seen: Set<String> = []
    var participants: [Participant] = []
    for attendee in attendees {
      let name = attendee.displayName.trimmingCharacters(in: .whitespacesAndNewlines)
      guard !name.isEmpty else { continue }
      let email = attendee.email?.trimmingCharacters(in: .whitespacesAndNewlines)
        .lowercased()
      let key = email.flatMap { $0.isEmpty ? nil : "email:\($0)" } ?? "name:\(name.lowercased())"
      guard seen.insert(key).inserted else { continue }
      participants.append(
        Participant(
          id: UUID(derivedFrom: meetingID, salt: "attendee-\(participants.count)"),
          meetingID: meetingID,
          displayName: name,
          role: .them,
          email: email.flatMap { $0.isEmpty ? nil : $0 }))
    }
    return participants
  }
}
