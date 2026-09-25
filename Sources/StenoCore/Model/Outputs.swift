import Foundation

public enum TaskPriority: String, Codable, Sendable, Equatable, Hashable, CaseIterable {
  case low
  case normal
  case high
}

/// A task the LLM extracted from the meeting.
public struct MeetingTask: Codable, Sendable, Equatable, Hashable, Identifiable {
  public var id: UUID
  public var meetingID: UUID
  public var text: String
  public var assigneePersonID: UUID?
  public var assigneeName: String?
  public var priority: TaskPriority
  public var dueDate: Date?
  public var done: Bool

  public init(
    id: UUID,
    meetingID: UUID,
    text: String,
    assigneePersonID: UUID? = nil,
    assigneeName: String? = nil,
    priority: TaskPriority = .normal,
    dueDate: Date? = nil,
    done: Bool = false
  ) {
    self.id = id
    self.meetingID = meetingID
    self.text = text
    self.assigneePersonID = assigneePersonID
    self.assigneeName = assigneeName
    self.priority = priority
    self.dueDate = dueDate
    self.done = done
  }
}

/// A decision the LLM extracted from the meeting.
public struct Decision: Codable, Sendable, Equatable, Hashable, Identifiable {
  public var id: UUID
  public var meetingID: UUID
  public var text: String

  public init(id: UUID, meetingID: UUID, text: String) {
    self.id = id
    self.meetingID = meetingID
    self.text = text
  }
}

// MARK: - Language model wire types

public enum LLMRole: String, Codable, Sendable, Equatable, Hashable {
  case system
  case user
  case assistant
}

public struct LLMMessage: Codable, Sendable, Equatable, Hashable {
  public var role: LLMRole
  public var content: String

  public init(role: LLMRole, content: String) {
    self.role = role
    self.content = content
  }
}

public enum LLMResponseFormat: Codable, Sendable, Equatable, Hashable {
  case text
  case jsonObject
  case jsonSchema(name: String, schema: JSONValue, strict: Bool)

  private struct Schema: Codable {
    var name: String
    var schema: JSONValue
    var strict: Bool
  }

  public init(from decoder: any Decoder) throws {
    let (name, payload) = try CaseCoding.decode(from: decoder)
    switch name {
    case "text": self = .text
    case "jsonObject": self = .jsonObject
    case "jsonSchema":
      let schema = try CaseCoding.decodePayload(Schema.self, from: payload, case: name)
      self = .jsonSchema(name: schema.name, schema: schema.schema, strict: schema.strict)
    default: throw CaseCoding.unknownCase(name, in: decoder)
    }
  }

  public func encode(to encoder: any Encoder) throws {
    switch self {
    case .text: try CaseCoding.encode("text", to: encoder)
    case .jsonObject: try CaseCoding.encode("jsonObject", to: encoder)
    case .jsonSchema(let name, let schema, let strict):
      try CaseCoding.encode(
        "jsonSchema", payload: Schema(name: name, schema: schema, strict: strict), to: encoder)
    }
  }
}

/// One completion request to an OpenAI-compatible endpoint. `purpose` names
/// the pass ("cleanup", "summary") for logs and usage accounting.
public struct LLMRequest: Codable, Sendable, Equatable, Hashable {
  public var messages: [LLMMessage]
  public var responseFormat: LLMResponseFormat
  public var temperature: Double?
  public var maxTokens: Int?
  public var purpose: String

  public init(
    messages: [LLMMessage],
    responseFormat: LLMResponseFormat = .text,
    temperature: Double? = nil,
    maxTokens: Int? = nil,
    purpose: String
  ) {
    self.messages = messages
    self.responseFormat = responseFormat
    self.temperature = temperature
    self.maxTokens = maxTokens
    self.purpose = purpose
  }
}

public enum LLMFinishReason: String, Codable, Sendable, Equatable, Hashable {
  case stop
  case length
  case contentFilter
  case other
}

public struct LLMResponse: Codable, Sendable, Equatable, Hashable {
  public var text: String
  public var finishReason: LLMFinishReason
  public var usage: LLMUsage?
  public var model: String?

  public init(
    text: String, finishReason: LLMFinishReason, usage: LLMUsage? = nil, model: String? = nil
  ) {
    self.text = text
    self.finishReason = finishReason
    self.usage = usage
    self.model = model
  }
}

// MARK: - Pipeline stage boundaries

/// Input of the cleanup stage: the merged transcript plus everything that
/// helps the model fix names.
public struct CleanupInput: Sendable, Equatable {
  public var segments: [TranscriptSegment]
  public var language: Locale.Language?
  public var participants: [Participant]
  public var speakers: [Speaker]
  public var knownPeople: [Person]

  public init(
    segments: [TranscriptSegment],
    language: Locale.Language?,
    participants: [Participant],
    speakers: [Speaker],
    knownPeople: [Person]
  ) {
    self.segments = segments
    self.language = language
    self.participants = participants
    self.speakers = speakers
    self.knownPeople = knownPeople
  }
}

/// Output of the cleanup stage: the same segments in the same order with
/// `text` rewritten; `failedChunks` lists chunk indexes left as raw text.
public struct CleanupOutput: Sendable, Equatable {
  public var segments: [TranscriptSegment]
  public var failedChunks: [Int]
  public var usage: LLMUsage

  public init(segments: [TranscriptSegment], failedChunks: [Int] = [], usage: LLMUsage) {
    self.segments = segments
    self.failedChunks = failedChunks
    self.usage = usage
  }
}

/// Input of the summarize stage.
public struct SummaryInput: Sendable, Equatable {
  public var meeting: Meeting
  public var segments: [TranscriptSegment]
  public var speakers: [Speaker]
  public var participants: [Participant]
  public var knownPeople: [Person]
  public var template: SummaryTemplate

  public init(
    meeting: Meeting,
    segments: [TranscriptSegment],
    speakers: [Speaker],
    participants: [Participant],
    knownPeople: [Person],
    template: SummaryTemplate
  ) {
    self.meeting = meeting
    self.segments = segments
    self.speakers = speakers
    self.participants = participants
    self.knownPeople = knownPeople
    self.template = template
  }
}

/// Output of the summarize stage, in the meeting language.
public struct SummaryOutput: Codable, Sendable, Equatable, Hashable {
  public var title: String
  public var summary: SummaryDocument
  public var decisions: [String]
  public var tasks: [MeetingTask]
  public var speakerNames: [SpeakerNameSuggestion]
  @LanguageTag public var language: Locale.Language?
  public var usage: LLMUsage

  public init(
    title: String,
    summary: SummaryDocument,
    decisions: [String],
    tasks: [MeetingTask],
    speakerNames: [SpeakerNameSuggestion],
    language: Locale.Language? = nil,
    usage: LLMUsage
  ) {
    self.title = title
    self.summary = summary
    self.decisions = decisions
    self.tasks = tasks
    self.speakerNames = speakerNames
    self.language = language
    self.usage = usage
  }
}

/// The model's guess who a speaker is, from conversational context. Never
/// applied automatically; the review sheet prefills from it.
public struct SpeakerNameSuggestion: Codable, Sendable, Equatable, Hashable {
  public var speakerID: UUID
  public var name: String?
  public var confidence: Double
  public var evidence: String

  public init(speakerID: UUID, name: String?, confidence: Double, evidence: String) {
    self.speakerID = speakerID
    self.name = name
    self.confidence = confidence
    self.evidence = evidence
  }
}

/// Everything an adapter receives; its `StenoJSON` encoding is `meeting.json`
/// everywhere.
public struct MeetingExport: Codable, Sendable, Equatable, Hashable {
  public static let currentSchemaVersion = 1

  public var schemaVersion: Int
  public var meeting: Meeting
  public var participants: [Participant]
  public var speakers: [Speaker]
  public var persons: [Person]
  public var segments: [TranscriptSegment]
  public var tasks: [MeetingTask]
  public var decisions: [Decision]
  public var audio: AudioAsset?

  public init(
    schemaVersion: Int = MeetingExport.currentSchemaVersion,
    meeting: Meeting,
    participants: [Participant],
    speakers: [Speaker],
    persons: [Person],
    segments: [TranscriptSegment],
    tasks: [MeetingTask],
    decisions: [Decision],
    audio: AudioAsset?
  ) {
    self.schemaVersion = schemaVersion
    self.meeting = meeting
    self.participants = participants
    self.speakers = speakers
    self.persons = persons
    self.segments = segments
    self.tasks = tasks
    self.decisions = decisions
    self.audio = audio
  }

  public func speaker(id: UUID) -> Speaker? {
    speakers.first { $0.id == id }
  }

  public func person(id: UUID) -> Person? {
    persons.first { $0.id == id }
  }

  /// The confirmed or suggested person's name, else the cluster label.
  public func displayName(forSpeaker id: UUID) -> String {
    guard let speaker = speaker(id: id) else { return "Unknown" }
    if let personID = speaker.personID, let person = person(id: personID) {
      return person.displayName
    }
    return speaker.clusterLabel
  }
}
