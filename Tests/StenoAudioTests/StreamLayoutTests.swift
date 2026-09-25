import StenoCore
import Testing

@testable import StenoAudio

@Suite struct StreamLayoutTests {
  /// Built-in speakers (no input), built-in mic (mono), interleaved stereo tap.
  @Test func callWithSeparateMicAndInterleavedTap() throws {
    let layout = try StreamLayout.resolve(
      lanes: [.mic, .system], aggregate: [1, 2], subDevices: [[], [1]], tap: [2], micSubDevice: 1)
    #expect(!layout.tapFirst)
    #expect(
      layout.sources == [
        .init(lane: .mic, bufferIndex: 0, channelOffset: 0, stride: 1),
        .init(
          lane: .system, bufferIndex: 1, channelOffset: 0, stride: 2, secondBufferIndex: 1,
          secondChannelOffset: 1),
      ])
  }

  /// The HAL puts the tap first and delivers it non-interleaved.
  @Test func tapFirstNonInterleaved() throws {
    let layout = try StreamLayout.resolve(
      lanes: [.mic, .system], aggregate: [1, 1, 2], subDevices: [[], [2]], tap: [1, 1],
      micSubDevice: 1)
    #expect(layout.tapFirst)
    #expect(layout.sources[0] == .init(lane: .mic, bufferIndex: 2, channelOffset: 0, stride: 2))
    #expect(
      layout.sources[1]
        == .init(
          lane: .system, bufferIndex: 0, channelOffset: 0, stride: 1, secondBufferIndex: 1,
          secondChannelOffset: 0))
  }

  /// A USB interface is both the output device and the microphone: one
  /// sub-device carrying two mono input streams.
  @Test func sharedInputOutputDevice() throws {
    let layout = try StreamLayout.resolve(
      lanes: [.mic, .system], aggregate: [1, 1, 2], subDevices: [[1, 1]], tap: [2], micSubDevice: 0)
    #expect(layout.sources[0] == .init(lane: .mic, bufferIndex: 0, channelOffset: 0, stride: 1))
    #expect(layout.sources[1].bufferIndex == 2)
  }

  /// The tap first and interleaved, the microphone mono.
  @Test func tapFirstInterleaved() throws {
    let layout = try StreamLayout.resolve(
      lanes: [.mic, .system], aggregate: [2, 1], subDevices: [[], [1]], tap: [2], micSubDevice: 1)
    #expect(layout.tapFirst)
    #expect(
      layout.sources == [
        .init(lane: .mic, bufferIndex: 1, channelOffset: 0, stride: 1),
        .init(
          lane: .system, bufferIndex: 0, channelOffset: 0, stride: 2, secondBufferIndex: 0,
          secondChannelOffset: 1),
      ])
  }

  /// A stereo microphone next to a stereo tap has the same shape in either
  /// order. The resolver then assumes sub-devices first; spike S2 on hardware
  /// confirms or refutes that assumption, and `capture-spike` prints it.
  @Test func equalShapesAssumeSubDevicesFirst() throws {
    let layout = try StreamLayout.resolve(
      lanes: [.mic, .system], aggregate: [2, 2], subDevices: [[], [2]], tap: [2], micSubDevice: 1)
    #expect(!layout.tapFirst)
    #expect(layout.sources[0] == .init(lane: .mic, bufferIndex: 0, channelOffset: 0, stride: 2))
    #expect(layout.sources[1].bufferIndex == 1)
    #expect(layout.sources[1].isMixed)
  }

  @Test func inPersonUsesTheMicOnly() throws {
    let layout = try StreamLayout.resolve(
      lanes: [.mixed], aggregate: [1], subDevices: [[], [1]], tap: [], micSubDevice: 1)
    #expect(layout.sources == [.init(lane: .mixed, bufferIndex: 0, channelOffset: 0, stride: 1)])
  }

  @Test func systemOnlySpikeWithMonoTap() throws {
    let layout = try StreamLayout.resolve(
      lanes: [.system], aggregate: [1], subDevices: [[]], tap: [1], micSubDevice: nil)
    #expect(layout.sources == [.init(lane: .system, bufferIndex: 0, channelOffset: 0, stride: 1)])
  }

  @Test func mismatchedShapesThrow() {
    #expect(throws: CaptureError.self) {
      try StreamLayout.resolve(
        lanes: [.mic, .system], aggregate: [2, 2], subDevices: [[], [1]], tap: [2], micSubDevice: 1)
    }
    #expect(throws: CaptureError.self) {
      try StreamLayout.resolve(
        lanes: [.mic], aggregate: [2], subDevices: [[]], tap: [2], micSubDevice: nil)
    }
    #expect(throws: CaptureError.self) {
      try StreamLayout.resolve(
        lanes: [.system], aggregate: [1], subDevices: [[1]], tap: [], micSubDevice: 0)
    }
  }
}
