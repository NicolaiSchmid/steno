import Foundation
import StenoCore

/// Speaker ids to the cluster labels the model sees ("Speaker 1"), and back.
/// Labels are what prompts and answers carry; StenoCore's renderer swaps
/// them for names later, so the model never needs to know a person's id.
public struct SpeakerLabels: Sendable, Equatable {
  public static let unknown = "Unknown speaker"

  private var labelsByID: [UUID: String]
  private var idsByLabel: [String: UUID]
  /// In `Speaker` order.
  public private(set) var ordered: [(id: UUID, label: String)]

  public init(speakers: [Speaker]) {
    labelsByID = [:]
    idsByLabel = [:]
    ordered = []
    for speaker in speakers {
      labelsByID[speaker.id] = speaker.clusterLabel
      idsByLabel[speaker.clusterLabel.lowercased()] = speaker.id
      ordered.append((speaker.id, speaker.clusterLabel))
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

  public static func == (lhs: SpeakerLabels, rhs: SpeakerLabels) -> Bool {
    lhs.labelsByID == rhs.labelsByID
  }
}

/// The transcript as the model reads it: `[n] Speaker 1: text`, one line
/// per segment, `n` counting from `startIndex`.
public enum TranscriptLines {
  public static func render(
    _ segments: [TranscriptSegment], labels: SpeakerLabels, startIndex: Int = 0
  ) -> String {
    segments.enumerated().map { offset, segment in
      "[\(startIndex + offset)] \(labels.label(for: segment.speakerID)): \(segment.text)"
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
