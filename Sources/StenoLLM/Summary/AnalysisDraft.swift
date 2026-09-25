import Foundation
import StenoCore

// The model's raw answers for the summary pass, before post-processing into
// `SummaryOutput`. Decoding into these types is the validation.

/// One task as the model wrote it. `dueDate` is `YYYY-MM-DD` or null and is
/// validated by Steno, never trusted.
public struct DraftTask: Codable, Sendable, Equatable {
  public enum Priority: String, Codable, Sendable, Equatable {
    case low, normal, high

    public var taskPriority: TaskPriority {
      switch self {
      case .low: .low
      case .normal: .normal
      case .high: .high
      }
    }
  }

  public var text: String
  public var assignee: String?
  public var priority: Priority
  public var dueDate: String?

  public init(text: String, assignee: String?, priority: Priority, dueDate: String?) {
    self.text = text
    self.assignee = assignee
    self.priority = priority
    self.dueDate = dueDate
  }
}

/// The model's guess who a speaker label is, with a quote as evidence.
public struct DraftSpeakerName: Codable, Sendable, Equatable {
  public var speakerLabel: String
  public var name: String?
  public var confidence: Double
  public var evidence: String

  public init(speakerLabel: String, name: String?, confidence: Double, evidence: String) {
    self.speakerLabel = speakerLabel
    self.name = name
    self.confidence = confidence
    self.evidence = evidence
  }
}

/// The single-shot or reduce answer.
public struct AnalysisDraft: Codable, Sendable, Equatable {
  public struct Bullet: Codable, Sendable, Equatable {
    public var lead: String
    public var text: String

    public init(lead: String, text: String) {
      self.lead = lead
      self.text = text
    }
  }

  public struct Section: Codable, Sendable, Equatable {
    public var id: String
    public var heading: String
    public var bullets: [Bullet]

    public init(id: String, heading: String, bullets: [Bullet]) {
      self.id = id
      self.heading = heading
      self.bullets = bullets
    }
  }

  public var title: String
  public var language: String
  public var sections: [Section]
  public var decisions: [String]
  public var tasks: [DraftTask]
  public var speakerNames: [DraftSpeakerName]

  public init(
    title: String, language: String, sections: [Section], decisions: [String],
    tasks: [DraftTask], speakerNames: [DraftSpeakerName]
  ) {
    self.title = title
    self.language = language
    self.sections = sections
    self.decisions = decisions
    self.tasks = tasks
    self.speakerNames = speakerNames
  }
}

/// The map answer for one chunk of a long transcript.
public struct ChunkNotes: Codable, Sendable, Equatable {
  public struct Topic: Codable, Sendable, Equatable {
    public var topic: String
    public var points: [String]

    public init(topic: String, points: [String]) {
      self.topic = topic
      self.points = points
    }
  }

  public var chunkIndex: Int
  public var topics: [Topic]
  public var decisions: [String]
  public var taskCandidates: [DraftTask]
  public var speakerCues: [DraftSpeakerName]

  public init(
    chunkIndex: Int, topics: [Topic], decisions: [String], taskCandidates: [DraftTask],
    speakerCues: [DraftSpeakerName]
  ) {
    self.chunkIndex = chunkIndex
    self.topics = topics
    self.decisions = decisions
    self.taskCandidates = taskCandidates
    self.speakerCues = speakerCues
  }
}
