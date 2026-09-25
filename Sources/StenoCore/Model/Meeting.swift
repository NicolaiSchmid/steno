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

  public var isFailed: Bool {
    if case .failed = self { return true }
    return false
  }

  public init(from decoder: any Decoder) throws {
    let (name, payload) = try CaseCoding.decode(from: decoder)
    switch name {
    case "recording": self = .recording
    case "queued": self = .queued
    case "processing": self = .processing
    case "ready": self = .ready
    case "failed":
      self = .failed(reason: try CaseCoding.decodePayload(String.self, from: payload, case: name))
    default: throw CaseCoding.unknownCase(name, in: decoder)
    }
  }

  public func encode(to encoder: any Encoder) throws {
    switch self {
    case .recording: try CaseCoding.encode("recording", to: encoder)
    case .queued: try CaseCoding.encode("queued", to: encoder)
    case .processing: try CaseCoding.encode("processing", to: encoder)
    case .ready: try CaseCoding.encode("ready", to: encoder)
    case .failed(let reason): try CaseCoding.encode("failed", payload: reason, to: encoder)
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
  @LanguageTag public var language: Locale.Language?
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
    language: Locale.Language? = nil,
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

/// Token accounting summed over every LLM call of a meeting.
public struct LLMUsage: Codable, Sendable, Equatable, Hashable {
  public var promptTokens: Int
  public var completionTokens: Int
  public var requests: Int

  public init(promptTokens: Int, completionTokens: Int, requests: Int) {
    self.promptTokens = promptTokens
    self.completionTokens = completionTokens
    self.requests = requests
  }

  public static let zero = LLMUsage(promptTokens: 0, completionTokens: 0, requests: 0)

  public static func + (lhs: LLMUsage, rhs: LLMUsage) -> LLMUsage {
    LLMUsage(
      promptTokens: lhs.promptTokens + rhs.promptTokens,
      completionTokens: lhs.completionTokens + rhs.completionTokens,
      requests: lhs.requests + rhs.requests
    )
  }
}
