import StenoCore

/// Where each lane's samples sit in the aggregate's input `AudioBufferList`.
/// Resolved once at start from the channel count of every buffer, so the
/// IOProc only follows precomputed indices. Pure, so the two possible HAL
/// orderings (sub-devices then taps, or taps first) are tested on CI.
public struct StreamLayout: Sendable, Equatable {
  /// One lane's source: the buffer index and channel offset of the first
  /// channel, the buffer's channel count as the sample stride, and an optional
  /// second channel (a stereo tap folded to mono).
  public struct LaneSource: Sendable, Equatable {
    public var lane: AudioLane
    public var bufferIndex: Int
    public var channelOffset: Int
    public var stride: Int
    /// -1 when the lane is a single channel.
    public var secondBufferIndex: Int = -1
    public var secondChannelOffset: Int = 0

    public var isMixed: Bool { secondBufferIndex >= 0 }
  }

  public var sources: [LaneSource]
  /// True when the HAL placed the tap's streams before the sub-devices'.
  public var tapFirst: Bool

  /// `aggregate`: channel count per input buffer of the aggregate device.
  /// `subDevices`: the same per sub-device, in sub-device order (the main
  /// output device first, then the microphone when it is a different device).
  /// `tap`: the tap's buffers (`[2]` interleaved stereo, `[1, 1]`
  /// non-interleaved, `[]` when there is no tap). `micSubDevice` is the index
  /// into `subDevices` whose first channel is the microphone, nil when no lane
  /// needs it.
  public static func resolve(
    lanes: [AudioLane], aggregate: [Int], subDevices: [[Int]], tap: [Int], micSubDevice: Int?
  ) throws -> StreamLayout {
    let flatSubDevices = subDevices.flatMap { $0 }
    let orderings: [(tapFirst: Bool, expected: [Int])] = [
      (false, flatSubDevices + tap), (true, tap + flatSubDevices),
    ]
    guard let ordering = orderings.first(where: { $0.expected == aggregate }) else {
      throw CaptureError.unexpectedStreamLayout(
        "aggregate buffers \(aggregate), sub-devices \(subDevices), tap \(tap)")
    }
    let tapOffset = ordering.tapFirst ? 0 : flatSubDevices.count
    let subDeviceOffset = ordering.tapFirst ? tap.count : 0

    var sources: [LaneSource] = []
    for lane in lanes {
      switch lane {
      case .mic, .mixed:
        guard let micSubDevice, micSubDevice < subDevices.count,
          let firstBuffer = subDevices[micSubDevice].first
        else {
          throw CaptureError.unexpectedStreamLayout("no microphone sub-device for \(lane)")
        }
        let start = subDeviceOffset + subDevices.prefix(micSubDevice).reduce(0) { $0 + $1.count }
        sources.append(
          LaneSource(lane: lane, bufferIndex: start, channelOffset: 0, stride: firstBuffer))
      case .system:
        guard !tap.isEmpty else {
          throw CaptureError.unexpectedStreamLayout("no tap buffers for the system lane")
        }
        if tap[0] >= 2 {
          sources.append(
            LaneSource(
              lane: lane, bufferIndex: tapOffset, channelOffset: 0, stride: tap[0],
              secondBufferIndex: tapOffset, secondChannelOffset: 1))
        } else if tap.count >= 2 {
          sources.append(
            LaneSource(
              lane: lane, bufferIndex: tapOffset, channelOffset: 0, stride: 1,
              secondBufferIndex: tapOffset + 1, secondChannelOffset: 0))
        } else {
          sources.append(
            LaneSource(lane: lane, bufferIndex: tapOffset, channelOffset: 0, stride: 1))
        }
      }
    }
    return StreamLayout(sources: sources, tapFirst: ordering.tapFirst)
  }
}
