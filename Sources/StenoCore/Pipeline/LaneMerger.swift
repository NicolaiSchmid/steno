import Foundation

/// Pure lane merge: one ordered transcript from per-lane raw segments.
/// `.mic` segments belong to the deterministic "me" speaker; `.system` and
/// `.mixed` segments get the diarization cluster covering their midpoint,
/// and a `.mixed` segment is never assigned to "me".
public enum LaneMerger {
  /// A diarization cluster and the `Speaker` row it became.
  public struct ClusterSpeaker: Sendable, Equatable {
    public var speakerID: UUID
    public var ranges: [ClosedRange<TimeInterval>]

    public init(speakerID: UUID, ranges: [ClosedRange<TimeInterval>]) {
      self.speakerID = speakerID
      self.ranges = ranges
    }
  }

  public static let meSpeakerLabel = "Me"

  /// The "me" speaker's id for a meeting, stable across re-runs.
  public static func meSpeakerID(meetingID: UUID) -> UUID {
    MeetingStore.derivedID(meetingID, salt: "speaker-me")
  }

  /// The "me" participant's id for a meeting.
  public static func meParticipantID(meetingID: UUID) -> UUID {
    MeetingStore.derivedID(meetingID, salt: "participant-me")
  }

  /// The `Speaker` row behind the mic lane: confirmed when the "me"
  /// participant is a known person, otherwise unknown with the label `Me`.
  public static func meSpeaker(meetingID: UUID, personID: UUID?) -> Speaker {
    Speaker(
      id: meSpeakerID(meetingID: meetingID),
      meetingID: meetingID,
      clusterLabel: meSpeakerLabel,
      assignment: personID.map { .confirmed(personID: $0) } ?? .unknown,
      clusterConfidence: 1
    )
  }

  /// Deterministic segment id from meeting, lane and index.
  public static func segmentID(meetingID: UUID, lane: AudioLane, index: Int) -> UUID {
    MeetingStore.derivedID(meetingID, salt: "segment-\(lane.rawValue)-\(index)")
  }

  public static func merge(
    meetingID: UUID,
    lanes: [AudioLane: [RawSegment]],
    clusters: [ClusterSpeaker],
    meSpeakerID: UUID?
  ) -> [TranscriptSegment] {
    var merged: [(order: (TimeInterval, Int, Int), segment: TranscriptSegment)] = []
    for (laneIndex, lane) in AudioLane.allCases.enumerated() {
      guard let raw = lanes[lane] else { continue }
      for (index, segment) in raw.enumerated() {
        let speakerID: UUID?
        switch lane {
        case .mic:
          speakerID = meSpeakerID
        case .system, .mixed:
          speakerID = cluster(covering: segment, in: clusters)
        }
        let transcript = TranscriptSegment(
          id: segmentID(meetingID: meetingID, lane: lane, index: index),
          meetingID: meetingID,
          start: segment.start,
          end: segment.end,
          speakerID: speakerID,
          lane: lane,
          text: segment.text,
          rawText: segment.text
        )
        merged.append(((segment.start, laneIndex, index), transcript))
      }
    }
    merged.sort { $0.order < $1.order }
    return merged.map(\.segment)
  }

  /// The cluster whose ranges contain the segment's midpoint; when none
  /// does, the cluster overlapping it the most; nil when none overlaps.
  static func cluster(covering segment: RawSegment, in clusters: [ClusterSpeaker]) -> UUID? {
    let midpoint = (segment.start + segment.end) / 2
    if let exact = clusters.first(where: { $0.ranges.contains { $0.contains(midpoint) } }) {
      return exact.speakerID
    }
    var best: (id: UUID, overlap: TimeInterval)?
    for cluster in clusters {
      let overlap = cluster.ranges.reduce(0.0) { total, range in
        total + max(0, min(range.upperBound, segment.end) - max(range.lowerBound, segment.start))
      }
      if overlap > 0, overlap > (best?.overlap ?? 0) {
        best = (cluster.speakerID, overlap)
      }
    }
    return best?.id
  }
}
