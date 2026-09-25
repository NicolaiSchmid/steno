import Foundation
import StenoCore

/// Speaker ids to the cluster labels the model sees ("Speaker 1"), and back.
/// Labels are what prompts and answers carry; StenoCore's renderer swaps
/// them for names later, so the model never needs to know a person's id.
public struct SpeakerLabels: Sendable, Equatable {
  static let unknown = "Unknown speaker"

  private var labelsByID: [UUID: String]
  private var idsByLabel: [String: UUID]

  public init(speakers: [Speaker]) {
    labelsByID = [:]
    idsByLabel = [:]
    for speaker in speakers {
      labelsByID[speaker.id] = speaker.clusterLabel
      idsByLabel[speaker.clusterLabel.lowercased()] = speaker.id
    }
  }

  public func label(for speakerID: UUID?) -> String {
    guard let speakerID, let label = labelsByID[speakerID] else { return Self.unknown }
    return label
  }

  /// The speaker whose label matches, case-insensitively and ignoring
  /// surrounding whitespace; nil for unknown labels.
  public func speakerID(forLabel label: String) -> UUID? {
    idsByLabel[label.trimmingCharacters(in: .whitespaces).lowercased()]
  }
}

/// The transcript as the model reads it: `[n] Speaker 1: text`, one line
/// per segment, `n` counting from 0.
public enum TranscriptLines {
  public static func render(_ segments: [TranscriptSegment], labels: SpeakerLabels) -> String {
    segments.enumerated().map { offset, segment in
      "[\(offset)] \(labels.label(for: segment.speakerID)): \(segment.text)"
    }.joined(separator: "\n")
  }

  /// Lines without indices for the summary pass, where nothing is mapped
  /// back by position: `Speaker 1: text`.
  public static func renderPlain(_ segments: [TranscriptSegment], labels: SpeakerLabels) -> String {
    segments.map { segment in
      "\(labels.label(for: segment.speakerID)): \(segment.text)"
    }.joined(separator: "\n")
  }
}
