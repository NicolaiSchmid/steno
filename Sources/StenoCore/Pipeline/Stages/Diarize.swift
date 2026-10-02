import Foundation

extension ProcessingPipeline {
  /// What the diarize stage hands on: the `Speaker` rows and, joined once,
  /// the cluster ranges each speaker covers for the lane merge.
  struct Diarization: Sendable {
    var speakers: [Speaker]
    var clusterSpeakers: [LaneMerger.ClusterSpeaker]
    /// The lane the clusters cover; nil when nothing was diarized.
    var lane: AudioLane? = nil
  }

  /// Which lane carries the voices to diarize: the tap in a call, else the
  /// one room lane.
  static func diarizedLane(source: MeetingSource, lanes: [AudioLane]) -> AudioLane? {
    let preferred: AudioLane = source == .macCall ? .system : .mixed
    if lanes.contains(preferred) { return preferred }
    return orderedLanes(lanes).last
  }

  /// `diarizedLane(source:lanes:)`, except for a call whose tap carried no
  /// conversation: then the microphone heard everyone (a phone on speaker
  /// next to the Mac, a call app the tap missed) and the mic lane is the
  /// room lane to diarize, instead of being "me" wholesale.
  static func diarizedLane(
    source: MeetingSource, lanes: [AudioLane], transcription: [AudioLane: [RawSegment]]
  ) -> AudioLane? {
    if source == .macCall, lanes.contains(.mic), tapCarriedNoConversation(transcription) {
      return .mic
    }
    return diarizedLane(source: source, lanes: lanes)
  }

  /// Speech on the tap below this share of the mic's speech means the tap
  /// carried no conversation. A notification chime or a hallucinated word
  /// on a silent tap stays under it; a real call partner never does.
  static let tapConversationMinimumShare: TimeInterval = 0.05

  /// True when the mic lane holds speech and the system lane holds less
  /// than `tapConversationMinimumShare` of it.
  static func tapCarriedNoConversation(_ lanes: [AudioLane: [RawSegment]]) -> Bool {
    guard let mic = lanes[.mic], let system = lanes[.system] else { return false }
    let micSpeech = mic.reduce(0.0) { $0 + $1.duration }
    let tapSpeech = system.reduce(0.0) { $0 + $1.duration }
    return micSpeech > 0 && tapSpeech < micSpeech * tapConversationMinimumShare
  }

  /// The sample clip is at most ten seconds.
  static let sampleClipSeconds: TimeInterval = 10

  /// Runs the diarizer (prepared by `process` before the run's first event)
  /// over the diarized lane and turns every cluster into a `Speaker` with a
  /// deterministic id, copying the cluster's embedding, confidence and
  /// sample clip range. Each cluster's clip is written as 16 kHz WAV to
  /// `RecordingLayout.sampleClip(speakerID:)` beside the master.
  ///
  /// `lane` is the lane to diarize, `diarizedLane(source:lanes:)` when nil.
  /// `buffer` is the last lane `decodeAndTranscribe` decoded, which is the
  /// diarized lane unless a call fell back to its mic lane, so the stage
  /// reuses it. The lane is decoded here only when no buffer was handed or
  /// it carries another lane; a caller who hands another lane's buffer holds
  /// two buffers for the stage's span.
  func diarize(
    asset: AudioAsset, meeting: Meeting, buffer handed: DecodedLane?, lane chosen: AudioLane? = nil
  ) async throws -> Diarization {
    let decoder = dependencies.decoder
    let diarizer = dependencies.diarizer
    let layout = RecordingLayout(asset: asset)
    return try await run(.diarize, meetingID: meeting.id) {
      guard let lane = chosen ?? Self.diarizedLane(source: meeting.source, lanes: asset.lanes)
      else {
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
      return Diarization(speakers: speakers, clusterSpeakers: clusterSpeakers, lane: lane)
    }
  }
}
