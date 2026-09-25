import Foundation

extension ProcessingPipeline {
  struct Merged: Sendable {
    var segments: [TranscriptSegment]
    var speakers: [Speaker]
  }

  /// Merges the lanes into one ordered transcript and persists it, together
  /// with the speakers and the meeting's elected language, in one
  /// transaction, so it survives a later failure. When the asset has a
  /// `.mic` lane, the "me" participant (created if the app did not write
  /// one) and the "me" speaker exist before any segment points at them.
  func merge(
    meeting: Meeting, lanes: [AudioLane: [RawSegment]], clusters: [SpeakerCluster],
    speakers: [Speaker]
  ) async throws -> Merged {
    let store = self.store
    return try await run(.merge, meetingID: meeting.id) {
      var allSpeakers = speakers
      var meSpeakerID: UUID?
      if lanes[.mic] != nil {
        let me = try await Self.ensureMeParticipant(meetingID: meeting.id, store: store)
        let meSpeaker = LaneMerger.meSpeaker(meetingID: meeting.id, personID: me.personID)
        allSpeakers.append(meSpeaker)
        meSpeakerID = meSpeaker.id
      }
      let clusterSpeakers = clusters.compactMap { cluster -> LaneMerger.ClusterSpeaker? in
        guard let speaker = speakers.first(where: { $0.clusterLabel == cluster.label }) else {
          return nil
        }
        return LaneMerger.ClusterSpeaker(speakerID: speaker.id, ranges: cluster.ranges)
      }
      let segments = LaneMerger.merge(
        meetingID: meeting.id, lanes: lanes, clusters: clusterSpeakers, meSpeakerID: meSpeakerID)
      var updated = meeting
      updated.updatedAt = self.now
      try await store.replaceTranscript(updated, segments: segments, speakers: allSpeakers)
      return Merged(segments: segments, speakers: allSpeakers)
    }
  }

  /// The participant with `role == .me`, created with the display name `Me`
  /// when the app wrote none.
  static func ensureMeParticipant(meetingID: UUID, store: MeetingStore) async throws -> Participant
  {
    if let existing = try await store.participants(meetingID: meetingID).first(where: {
      $0.role == .me
    }) {
      return existing
    }
    let me = Participant(
      id: LaneMerger.meParticipantID(meetingID: meetingID),
      meetingID: meetingID,
      displayName: LaneMerger.meSpeakerLabel,
      role: .me
    )
    try await store.save(me)
    return me
  }
}
