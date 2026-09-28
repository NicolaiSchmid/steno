import Foundation

/// Where a recording came from. Decides which lanes exist and which lane is
/// diarized.
public enum MeetingSource: String, Codable, Sendable, Equatable, Hashable, CaseIterable {
  /// Two lanes on the Mac: `.mic` is "me", `.system` is "them".
  case macCall
  /// One `.mixed` room lane from the Mac microphone, fully diarized.
  case macInPerson
  /// One `.mixed` lane recorded by the phone and handed over.
  case phone
}

/// The processing state machine: `recording → queued → processing → ready`,
/// or `failed(reason)` from any processing stage.
public enum MeetingState: Codable, Sendable, Equatable, Hashable {
  case recording
  case queued
  case processing
  case ready
  case failed(reason: String)

  /// The case names, shared by `meeting.json` and the `meeting.state` column.
  public enum Kind: String, CaseIterable, Codable, Sendable {
    case recording, queued, processing, ready, failed
  }

  public var kind: Kind {
    switch self {
    case .recording: .recording
    case .queued: .queued
    case .processing: .processing
    case .ready: .ready
    case .failed: .failed
    }
  }

  public var isFailed: Bool { kind == .failed }

  public init(from decoder: any Decoder) throws {
    let (kind, payload) = try CaseCoding.decode(Kind.self, from: decoder)
    switch kind {
    case .recording: self = .recording
    case .queued: self = .queued
    case .processing: self = .processing
    case .ready: self = .ready
    case .failed:
      self = .failed(reason: try CaseCoding.decodePayload(String.self, from: payload, case: kind))
    }
  }

  public func encode(to encoder: any Encoder) throws {
    switch self {
    case .failed(let reason): try CaseCoding.encode(kind, payload: reason, to: encoder)
    default: try CaseCoding.encode(kind, to: encoder)
    }
  }
}

/// Why a recording ended. Written by `LocalRecordingIntake.complete` from
/// `RecordingResult.endReason`; nil for meetings recorded before it was
/// stored and for phone recordings.
public enum RecordingEndReason: Codable, Sendable, Equatable, Hashable {
  /// The user stopped it from any surface.
  case manual
  /// The call app closed the microphone and the grace period ran out.
  /// `appName` is nil when the app could not be named.
  case callEnded(appName: String?)
  /// An audio device disappeared and did not come back within the retries.
  case deviceLost
  /// Steno quit while recording.
  case quit

  /// The case names, shared by `meeting.json` and the `meeting.endReason`
  /// column.
  public enum Kind: String, CaseIterable, Codable, Sendable {
    case manual, callEnded, deviceLost, quit
  }

  public var kind: Kind {
    switch self {
    case .manual: .manual
    case .callEnded: .callEnded
    case .deviceLost: .deviceLost
    case .quit: .quit
    }
  }

  /// `"manual"`, `{"callEnded":"Zen"}`; a nameless `.callEnded` is the bare
  /// `"callEnded"`, so the nil payload never becomes a JSON null (a null is
  /// still read as nil).
  public init(from decoder: any Decoder) throws {
    let (kind, payload) = try CaseCoding.decode(Kind.self, from: decoder)
    switch kind {
    case .manual: self = .manual
    case .callEnded:
      guard let payload else {
        self = .callEnded(appName: nil)
        return
      }
      self = .callEnded(appName: try String?(from: payload))
    case .deviceLost: self = .deviceLost
    case .quit: self = .quit
    }
  }

  public func encode(to encoder: any Encoder) throws {
    switch self {
    case .callEnded(let appName?): try CaseCoding.encode(kind, payload: appName, to: encoder)
    default: try CaseCoding.encode(kind, to: encoder)
    }
  }
}

/// Where `Meeting.title` came from, so a surface can tell the machine
/// default ("Call 2026-09-24 11:00") from a name somebody chose without
/// re-running the formatter.
public enum TitleOrigin: String, Codable, Sendable, Equatable, Hashable, CaseIterable {
  /// `LocalRecordingIntake.defaultTitle` or the phone intake's default.
  case `default`
  /// The overlapping calendar event's title.
  case calendar
  /// The model's title from the summarize stage.
  case summary
  /// Typed by the user (`MeetingStore.rename`).
  case user
}

/// One meeting: the row every other row hangs off.
public struct Meeting: Codable, Sendable, Equatable, Hashable, Identifiable {
  public var id: UUID
  public var title: String
  public var startedAt: Date
  /// Length of the recording in seconds.
  public var duration: TimeInterval
  /// Elected by the pipeline from tagged transcript segments; nil when
  /// untagged or not yet processed.
  public var language: LanguageTag?
  public var source: MeetingSource
  public var calendarEventID: String?
  public var tags: [String]
  public var state: MeetingState
  public var endReason: RecordingEndReason?
  /// Where `title` came from; `.default` when absent from JSON.
  public var titleOrigin: TitleOrigin
  public var templateID: String
  /// Nil on a `.ready` meeting means the summary was skipped: no LLM
  /// endpoint was configured when it was processed; see `PipelineDependencies`.
  public var summary: SummaryDocument?
  /// Free text typed by the user; the one editable text of a meeting.
  public var scratchpad: String
  public var llmUsage: LLMUsage?
  public var createdAt: Date
  public var updatedAt: Date

  public init(
    id: UUID,
    title: String,
    startedAt: Date,
    duration: TimeInterval,
    language: LanguageTag? = nil,
    source: MeetingSource,
    calendarEventID: String? = nil,
    tags: [String] = [],
    state: MeetingState,
    endReason: RecordingEndReason? = nil,
    titleOrigin: TitleOrigin = .default,
    templateID: String = SummaryTemplate.defaultID,
    summary: SummaryDocument? = nil,
    scratchpad: String = "",
    llmUsage: LLMUsage? = nil,
    createdAt: Date,
    updatedAt: Date
  ) {
    self.id = id
    self.title = title
    self.startedAt = startedAt
    self.duration = duration
    self.language = language
    self.source = source
    self.calendarEventID = calendarEventID
    self.tags = tags
    self.state = state
    self.endReason = endReason
    self.titleOrigin = titleOrigin
    self.templateID = templateID
    self.summary = summary
    self.scratchpad = scratchpad
    self.llmUsage = llmUsage
    self.createdAt = createdAt
    self.updatedAt = updatedAt
  }

  private enum CodingKeys: String, CodingKey {
    case id, title, startedAt, duration, language, source, calendarEventID, tags, state
    case endReason, titleOrigin, templateID, summary, scratchpad, llmUsage, createdAt, updatedAt
  }

  /// Hand-written for one rule the synthesised conformance cannot express:
  /// `titleOrigin` is omitted when `.default` and read as `.default` when
  /// absent, so `Tests/Fixtures/meetings/*.json`, the export golden and every
  /// `meeting.json` written before `v3` decode and re-encode unchanged. (A
  /// nil `endReason` would be omitted by the synthesised code too.) The
  /// cost: a new stored property needs a `CodingKeys` case, an `init(from:)`
  /// line and an `encode(to:)` line, and only the `SampleData.meeting()`
  /// round trip catches a missed one.
  public init(from decoder: any Decoder) throws {
    let container = try decoder.container(keyedBy: CodingKeys.self)
    id = try container.decode(UUID.self, forKey: .id)
    title = try container.decode(String.self, forKey: .title)
    startedAt = try container.decode(Date.self, forKey: .startedAt)
    duration = try container.decode(TimeInterval.self, forKey: .duration)
    language = try container.decodeIfPresent(LanguageTag.self, forKey: .language)
    source = try container.decode(MeetingSource.self, forKey: .source)
    calendarEventID = try container.decodeIfPresent(String.self, forKey: .calendarEventID)
    tags = try container.decode([String].self, forKey: .tags)
    state = try container.decode(MeetingState.self, forKey: .state)
    endReason = try container.decodeIfPresent(RecordingEndReason.self, forKey: .endReason)
    titleOrigin = try container.decodeIfPresent(TitleOrigin.self, forKey: .titleOrigin) ?? .default
    templateID = try container.decode(String.self, forKey: .templateID)
    summary = try container.decodeIfPresent(SummaryDocument.self, forKey: .summary)
    scratchpad = try container.decode(String.self, forKey: .scratchpad)
    llmUsage = try container.decodeIfPresent(LLMUsage.self, forKey: .llmUsage)
    createdAt = try container.decode(Date.self, forKey: .createdAt)
    updatedAt = try container.decode(Date.self, forKey: .updatedAt)
  }

  public func encode(to encoder: any Encoder) throws {
    var container = encoder.container(keyedBy: CodingKeys.self)
    try container.encode(id, forKey: .id)
    try container.encode(title, forKey: .title)
    try container.encode(startedAt, forKey: .startedAt)
    try container.encode(duration, forKey: .duration)
    try container.encodeIfPresent(language, forKey: .language)
    try container.encode(source, forKey: .source)
    try container.encodeIfPresent(calendarEventID, forKey: .calendarEventID)
    try container.encode(tags, forKey: .tags)
    try container.encode(state, forKey: .state)
    try container.encodeIfPresent(endReason, forKey: .endReason)
    if titleOrigin != .default { try container.encode(titleOrigin, forKey: .titleOrigin) }
    try container.encode(templateID, forKey: .templateID)
    try container.encodeIfPresent(summary, forKey: .summary)
    try container.encode(scratchpad, forKey: .scratchpad)
    try container.encodeIfPresent(llmUsage, forKey: .llmUsage)
    try container.encode(createdAt, forKey: .createdAt)
    try container.encode(updatedAt, forKey: .updatedAt)
  }

  /// Copies the columns the pipeline owns from `results`: title with its
  /// origin, language, state, templateID, summary, llmUsage, updatedAt.
  /// Everything else (`scratchpad`, `tags`, `calendarEventID`, `endReason`,
  /// source and timing) belongs to the user or the app and stays as stored,
  /// so a stage that read the meeting minutes ago cannot revert an edit made
  /// while it ran.
  public mutating func applyProcessingResults(_ results: Meeting) {
    title = results.title
    titleOrigin = results.titleOrigin
    language = results.language
    state = results.state
    templateID = results.templateID
    summary = results.summary
    llmUsage = results.llmUsage
    updatedAt = results.updatedAt
  }
}
