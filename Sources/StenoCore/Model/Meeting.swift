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
  public var templateID: String
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
    self.templateID = templateID
    self.summary = summary
    self.scratchpad = scratchpad
    self.llmUsage = llmUsage
    self.createdAt = createdAt
    self.updatedAt = updatedAt
  }

  /// Copies the columns the pipeline owns from `results`: title, language,
  /// state, templateID, summary, llmUsage, updatedAt. Everything else
  /// (`scratchpad`, `tags`, `calendarEventID`, source and timing) belongs to
  /// the user or the app and stays as stored, so a stage that read the meeting
  /// minutes ago cannot revert an edit made while it ran.
  public mutating func applyProcessingResults(_ results: Meeting) {
    title = results.title
    language = results.language
    state = results.state
    templateID = results.templateID
    summary = results.summary
    llmUsage = results.llmUsage
    updatedAt = results.updatedAt
  }
}
