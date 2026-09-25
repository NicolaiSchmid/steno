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

  /// Decodes the diarized lane again, runs the diarizer, and turns every
  /// cluster into a `Speaker` with a deterministic id, copying the cluster's
  /// embedding, confidence and sample clip range. Each cluster's clip is
  /// written as 16 kHz WAV to `RecordingLayout.sampleClip(speakerID:)`
  /// beside the master.
  func diarize(asset: AudioAsset, meeting: Meeting) async throws -> Diarization {
    let decoder = dependencies.decoder
    let diarizer = dependencies.diarizer
    let layout = RecordingLayout(asset: asset)
    return try await run(.diarize, meetingID: meeting.id) {
      guard let lane = Self.diarizedLane(source: meeting.source, lanes: asset.lanes) else {
        return Diarization(speakers: [], clusterSpeakers: [])
      }
      try await diarizer.prepare()
      let buffer = try await decoder.decode(asset, lane: lane)
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
