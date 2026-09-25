import Foundation
import StenoCore

// The model's raw answers for the summary pass, before post-processing into
// `SummaryOutput` (`AnalysisDraft+Output.swift`). Decoding into these types
// is the validation.

/// One task as the model wrote it. `dueDate` is `YYYY-MM-DD` or null and is
/// validated by Steno, never trusted; `priority` decodes straight into
/// core's closed enum, so an unknown value is a decode failure.
public struct DraftTask: Codable, Sendable, Equatable {
  public var text: String
  public var assignee: String?
  public var priority: TaskPriority
  public var dueDate: String?
}

/// The model's guess who a speaker label is, with a quote as evidence.
public struct DraftSpeakerName: Codable, Sendable, Equatable {
  public var speakerLabel: String
  public var name: String?
  public var confidence: Double
  public var evidence: String
}

/// The single-shot or reduce answer.
public struct AnalysisDraft: Codable, Sendable, Equatable {
  public struct Bullet: Codable, Sendable, Equatable {
    public var lead: String
    public var text: String
  }

  public struct Section: Codable, Sendable, Equatable {
    public var id: String
    public var heading: String
    public var bullets: [Bullet]
  }

  public var title: String
  public var sections: [Section]
  public var decisions: [String]
  public var tasks: [DraftTask]
  public var speakerNames: [DraftSpeakerName]
}

/// The map answer for one chunk of a long transcript.
public struct ChunkNotes: Codable, Sendable, Equatable {
  public struct Topic: Codable, Sendable, Equatable {
    public var topic: String
    public var points: [String]
  }

  public var chunkIndex: Int
  public var topics: [Topic]
  public var decisions: [String]
  public var taskCandidates: [DraftTask]
  public var speakerCues: [DraftSpeakerName]
}
