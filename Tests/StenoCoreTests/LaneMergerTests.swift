import Foundation
import Testing

@testable import StenoCore

@Suite struct LaneMergerTests {
  let meetingID = SampleData.meetingID
  var me: UUID { LaneMerger.meSpeakerID(meetingID: meetingID) }

  @Test func micSegmentsGetMeAndSystemSegmentsGetTheirCluster() {
    let a = SampleData.uuid(20)
    let b = SampleData.uuid(21)
    let clusters = [
      LaneMerger.ClusterSpeaker(speakerID: a, ranges: [0...1.5, 3...4.5]),
      LaneMerger.ClusterSpeaker(speakerID: b, ranges: [1.5...3]),
    ]
    let lanes: [AudioLane: [RawSegment]] = [
      .mic: [
        RawSegment(start: 0.2, end: 1, text: "me one"),
        RawSegment(start: 2, end: 2.5, text: "me two"),
      ],
      .system: [
        RawSegment(start: 0.5, end: 1.2, text: "a one"),
        RawSegment(start: 1.6, end: 2.9, text: "b one"),
        RawSegment(start: 3.1, end: 4, text: "a two"), RawSegment(start: 5, end: 6, text: "nobody"),
      ],
    ]
    let merged = LaneMerger.merge(
      meetingID: meetingID, lanes: lanes, clusters: clusters, meSpeakerID: me)
    #expect(merged.map(\.text) == ["me one", "a one", "b one", "me two", "a two", "nobody"])
    #expect(merged.map(\.speakerID) == [me, a, b, me, a, nil])
    #expect(merged.map(\.lane) == [.mic, .system, .system, .mic, .system, .system])
    #expect(merged.allSatisfy { $0.rawText == $0.text })
    #expect(merged[0].id == LaneMerger.segmentID(meetingID: meetingID, lane: .mic, index: 0))
    #expect(merged[1].id == LaneMerger.segmentID(meetingID: meetingID, lane: .system, index: 0))
    #expect(Set(merged.map(\.id)).count == merged.count)
  }

  @Test func overlapDecidesWhenNoClusterCoversTheMidpoint() {
    let a = SampleData.uuid(20)
    let b = SampleData.uuid(21)
    let clusters = [
      LaneMerger.ClusterSpeaker(speakerID: a, ranges: [0...1]),
      LaneMerger.ClusterSpeaker(speakerID: b, ranges: [1.8...3]),
    ]
    let segment = RawSegment(start: 0.5, end: 2.5, text: "straddles the gap")
    // Midpoint 1.5 is uncovered; a overlaps 0.5 s, b overlaps 0.7 s.
    #expect(LaneMerger.cluster(covering: segment, in: clusters) == b)
    #expect(
      LaneMerger.cluster(covering: RawSegment(start: 5, end: 6, text: "x"), in: clusters) == nil)
  }

  @Test func meSpeakerAndParticipantAreDeterministic() {
    let unknown = LaneMerger.meSpeaker(meetingID: meetingID, personID: nil)
    #expect(unknown.id == me)
    #expect(unknown.clusterLabel == "Me")
    #expect(unknown.assignment == .unknown)
    let known = LaneMerger.meSpeaker(meetingID: meetingID, personID: SampleData.personNicolaiID)
    #expect(known.assignment == .confirmed(personID: SampleData.personNicolaiID))
    #expect(LaneMerger.meParticipantID(meetingID: meetingID) != me)
    #expect(
      LaneMerger.meParticipantID(meetingID: meetingID)
        == LaneMerger.meParticipantID(meetingID: meetingID))
  }

  /// Property test over seeded random lanes: sorted by start, nothing lost,
  /// no `.mixed` segment carries the "me" id.
  @Test func mergePreservesEverySegmentInOrderAndNeverAssignsMeToMixed() {
    var generator = SplitMix64(seed: 2026)
    for round in 0..<200 {
      let laneSet: [AudioLane] =
        round % 3 == 0 ? [.mixed] : round % 3 == 1 ? [.mic, .system] : [.mic, .mixed]
      var lanes: [AudioLane: [RawSegment]] = [:]
      var total = 0
      for lane in laneSet {
        let count = Int(generator.next() % 8)
        var start = 0.0
        var segments: [RawSegment] = []
        for _ in 0..<count {
          let gap = Double(generator.next() % 20) / 10
          let length = 0.1 + Double(generator.next() % 30) / 10
          start += gap
          segments.append(
            RawSegment(start: start, end: start + length, text: "\(lane.rawValue) \(start)"))
          start += length
        }
        lanes[lane] = segments
        total += count
      }
      let clusterA = LaneMerger.ClusterSpeaker(
        speakerID: SampleData.uuid(20), ranges: [0...5, 10...12])
      let clusterB = LaneMerger.ClusterSpeaker(speakerID: SampleData.uuid(21), ranges: [5...10])
      let merged = LaneMerger.merge(
        meetingID: meetingID, lanes: lanes, clusters: [clusterA, clusterB],
        meSpeakerID: laneSet.contains(.mic) ? me : nil)

      #expect(merged.count == total, "round \(round) keeps every segment")
      #expect(
        merged == merged.sorted { $0.start < $1.start }
          || zip(merged, merged.dropFirst()).allSatisfy { $0.start <= $1.start },
        "round \(round) sorted by start")
      #expect(
        !merged.contains { $0.lane == .mixed && $0.speakerID == me },
        "round \(round) mixed never me")
      #expect(
        !merged.contains { $0.lane == .system && $0.speakerID == me },
        "round \(round) system never me")
      #expect(
        merged.filter { $0.lane == .mic }.allSatisfy { $0.speakerID == me },
        "round \(round) mic is me")
      #expect(Set(merged.map(\.id)).count == merged.count, "round \(round) unique ids")
    }
  }
}
