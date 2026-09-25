import Foundation

extension ProcessingPipeline {
  struct Diarization: Sendable {
    var speakers: [Speaker]
    var clusters: [SpeakerCluster]
  }

  /// Which lane carries the voices to diarize: the tap in a call, else the
  /// one room lane.
  static func diarizedLane(source: MeetingSource, lanes: [AudioLane]) -> AudioLane? {
    let preferred: AudioLane = source == .macCall ? .system : .mixed
    if lanes.contains(preferred) { return preferred }
    return orderedLanes(lanes).last
  }

  /// The sample clip is at most ten seconds.
  public static let sampleClipSeconds: TimeInterval = 10

  /// Decodes the diarized lane again, runs the diarizer, and turns every
  /// cluster into a `Speaker` with a deterministic id, copying the cluster's
  /// embedding, confidence and sample clip range. Each cluster's clip is
  /// written as 16 kHz WAV to `<meeting folder>/speakers/<speakerID>.wav`.
  func diarize(asset: AudioAsset, meeting: Meeting, settings: Settings) async throws -> Diarization
  {
    let decoder = dependencies.decoder
    let diarizer = dependencies.diarizer
    let folder = Self.meetingFolder(meeting.id, settings: settings)
      .appendingPathComponent("speakers", isDirectory: true)
    return try await run(.diarize, meetingID: meeting.id) {
      guard let lane = Self.diarizedLane(source: meeting.source, lanes: asset.lanes) else {
        return Diarization(speakers: [], clusters: [])
      }
      try await diarizer.prepare()
      let buffer = try await decoder.decode(asset, lane: lane)
      let result = try await diarizer.diarize(buffer)
      var speakers: [Speaker] = []
      for cluster in result.clusters {
        let id = MeetingStore.derivedID(meeting.id, salt: "speaker-\(cluster.label)")
        var clipURL: URL?
        if let range = cluster.sampleClipRange {
          let capped =
            range.lowerBound...min(range.upperBound, range.lowerBound + Self.sampleClipSeconds)
          let clip = buffer.slice(capped)
          if !clip.samples.isEmpty {
            try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
            let url = folder.appendingPathComponent("\(id.uuidString).wav")
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
      }
      return Diarization(speakers: speakers, clusters: result.clusters)
    }
  }
}
