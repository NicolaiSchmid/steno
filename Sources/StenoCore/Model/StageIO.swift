import Foundation

// The pipeline's stage boundaries: what StenoLLM's cleaner and summarizer
// receive and return, and the export every adapter receives.

/// Input of the cleanup stage: the merged transcript plus everything that
/// helps the model fix names.
public struct CleanupInput: Sendable, Equatable {
  public var segments: [TranscriptSegment]
  public var language: LanguageTag?
  public var participants: [Participant]
  public var speakers: [Speaker]
  public var knownPeople: [Person]

  public init(
    segments: [TranscriptSegment],
    language: LanguageTag?,
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
  public var language: LanguageTag?
  public var usage: LLMUsage

  public init(
    title: String,
    summary: SummaryDocument,
    decisions: [String],
    tasks: [MeetingTask],
    speakerNames: [SpeakerNameSuggestion],
    language: LanguageTag? = nil,
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
