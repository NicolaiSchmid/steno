import Foundation

extension ProcessingPipeline {
  /// What the diarize stage hands on: the `Speaker` rows and, joined once,
  /// the cluster ranges each speaker covers for the lane merge.
  struct Diarization: Sendable {
    var speakers: [Speaker]
    var clusterSpeakers: [LaneMerger.ClusterSpeaker]
  }

  /// Which lane carries the voices to diarize: the tap in a call, else the
  /// one room lane.
  static func diarizedLane(source: MeetingSource, lanes: [AudioLane]) -> AudioLane? {
    let preferred: AudioLane = source == .macCall ? .system : .mixed
    if lanes.contains(preferred) { return preferred }
    return orderedLanes(lanes).last
  }

  /// The sample clip is at most ten seconds.
  static let sampleClipSeconds: TimeInterval = 10

  /// Runs the diarizer (prepared by `process` before the run's first event)
  /// over the diarized lane and turns every cluster into a `Speaker` with a
  /// deterministic id, copying the cluster's embedding, confidence and
  /// sample clip range. Each cluster's clip is written as 16 kHz WAV to
  /// `RecordingLayout.sampleClip(speakerID:)` beside the master.
  ///
  /// `buffer` is the last lane `decodeAndTranscribe` decoded, which under
  /// today's lane rules is the diarized lane (`.system` for a call, the room
  /// lane otherwise, both last in `orderedLanes`), so the stage reuses it
  /// and the one-buffer invariant holds without a second decode. The lane
  /// is decoded here only when no buffer was handed or it carries another
  /// lane, a branch `process` never takes; a caller who does take it holds
  /// two buffers for the stage's span.
  func diarize(asset: AudioAsset, meeting: Meeting, buffer handed: DecodedLane?) async throws
    -> Diarization
  {
    let decoder = dependencies.decoder
    let diarizer = dependencies.diarizer
    let layout = RecordingLayout(asset: asset)
    return try await run(.diarize, meetingID: meeting.id) {
      guard let lane = Self.diarizedLane(source: meeting.source, lanes: asset.lanes) else {
        return Diarization(speakers: [], clusterSpeakers: [])
      }
      let buffer: AudioBuffer16k
      if let handed, handed.lane == lane {
        buffer = handed.buffer
      } else {
        buffer = try await decoder.decode(asset, lane: lane)
      }
      let result = try await diarizer.diarize(buffer)
      var labels = Set<String>()
      for cluster in result.clusters where !labels.insert(cluster.label).inserted {
        throw PipelineFailure(
          stage: .diarize, reason: "diarizer returned two clusters labelled \(cluster.label)")
      }
      var speakers: [Speaker] = []
      var clusterSpeakers: [LaneMerger.ClusterSpeaker] = []
      for cluster in result.clusters {
        let id = UUID(derivedFrom: meeting.id, salt: "speaker-\(cluster.label)")
        var clipURL: URL?
        if let range = cluster.sampleClipRange {
          let capped =
            range.lowerBound...min(range.upperBound, range.lowerBound + Self.sampleClipSeconds)
          let clip = buffer.slice(capped)
          if !clip.samples.isEmpty {
            try layout.createDirectories(speakers: true)
            let url = layout.sampleClip(speakerID: id)
            try WAVWriter.write(clip, to: url)
            clipURL = url
          }
        }
        speakers.append(
          Speaker(
            id: id,
            meetingID: meeting.id,
            clusterLabel: cluster.label,
            assignment: .unknown,
            embedding: cluster.embedding,
            sampleClipRange: cluster.sampleClipRange,
            sampleClipURL: clipURL,
            clusterConfidence: cluster.clusterConfidence
          ))
        clusterSpeakers.append(LaneMerger.ClusterSpeaker(speakerID: id, ranges: cluster.ranges))
      }
      return Diarization(speakers: speakers, clusterSpeakers: clusterSpeakers)
    }
  }
}
