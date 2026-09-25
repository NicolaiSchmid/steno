import Foundation
import StenoCore
import Testing

@testable import StenoAudio

/// Synthetic backend → sink rings → processing thread → relay, without a
/// session or files. Runs as fast as the rings allow.
@Suite struct ProcessingThreadTests {
  /// Drains the relay on the test thread and returns every frame per channel.
  func drain(_ relay: FrameRelay, channels: Int, frameSize: Int, expectedFrames: Int)
    -> [[Float]]
  {
    var output = [[Float]](repeating: [], count: channels)
    var scratch = [Float](repeating: 0, count: frameSize)
    var frames = 0
    var idle = 0
    while frames < expectedFrames, idle < 2_000 {
      if relay.availableFrames > 0 {
        for channel in 0..<channels {
          scratch.withUnsafeMutableBufferPointer {
            _ = relay.read(channel: channel, into: $0.baseAddress!)
          }
          output[channel].append(contentsOf: scratch)
        }
        frames += 1
        idle = 0
      } else {
        idle += 1
        Thread.sleep(forTimeInterval: 0.001)
      }
    }
    return output
  }

  func rmsDecibels(_ samples: ArraySlice<Float>) -> Float {
    let sum = samples.reduce(0.0) { $0 + Double($1 * $1) }
    return LevelMeter.decibels(Float((sum / Double(max(samples.count, 1))).squareRoot()))
  }

  @Test func tonesFlowThroughToTheRelayWithoutDrops() throws {
    let lanes: [AudioLane] = [.mic, .system]
    let backend = SyntheticCaptureBackend(
      lanes: lanes, tone: [.mic: 440, .system: 1_000], seconds: 1)
    let sink = LaneFrameSink(lanes: lanes)
    let relay = FrameRelay(channels: 2, frameSize: 480, capacityFrames: 400)
    let thread = ProcessingThread(
      sink: sink, relay: relay,
      configuration: .init(lanes: lanes, echoCanceller: nil))
    thread.start()
    try backend.start(lanes: lanes, inputDeviceUID: nil, sink: sink)

    let output = drain(relay, channels: 2, frameSize: 480, expectedFrames: 100)
    backend.stop()
    thread.stop()

    #expect(output[0].count == 48_000)
    #expect(output[1].count == 48_000)
    #expect(backend.framesDelivered == 48_000)
    #expect(thread.framesProcessed == 100)
    #expect(sink.droppedSamples.isEmpty)
    #expect(relay.droppedFrames == [0, 0])
    // Amplitude 0.5 sines: -9.03 dBFS rms on both lanes.
    #expect(abs(rmsDecibels(output[0][...]) - -9.03) < 0.1)
    #expect(abs(rmsDecibels(output[1][...]) - -9.03) < 0.1)
    // The 440 Hz lane crosses zero upward 440 times a second, the 1 kHz lane 1 000 times.
    func upwardCrossings(_ samples: [Float]) -> Int {
      zip(samples, samples.dropFirst()).filter { $0 < 0 && $1 >= 0 }.count
    }
    #expect(abs(upwardCrossings(output[0]) - 440) <= 1)
    #expect(abs(upwardCrossings(output[1]) - 1_000) <= 1)
    let levels = thread.levels.levels
    #expect(abs(levels.mic.rms - -9.03) < 0.2)
    #expect(abs((levels.system?.rms ?? 0) - -9.03) < 0.2)
    #expect(thread.levels.currentGeneration == 10, "10 Hz over one second")
    #expect(abs(thread.systemPeak - 0.5) < 0.01)
  }

  @Test func passthroughCancellerAndRawMicChannel() throws {
    let lanes: [AudioLane] = [.mic, .system]
    let backend = SyntheticCaptureBackend(
      signals: [
        .mic: SyntheticLane(frequency: 300, amplitude: 0.25),
        .system: SyntheticLane(frequency: 2_000, amplitude: 0.5),
      ], seconds: 0.5)
    let sink = LaneFrameSink(lanes: lanes)
    let relay = FrameRelay(channels: 3, frameSize: 480, capacityFrames: 100)
    let thread = ProcessingThread(
      sink: sink, relay: relay,
      configuration: .init(
        lanes: lanes,
        echoCanceller: try PassthroughEchoCanceller(sampleRate: 48_000, frameSize: 480),
        farEndDelayFrames: 2_880, keepRawMic: true))
    thread.start()
    try backend.start(lanes: lanes, inputDeviceUID: nil, sink: sink)
    let output = drain(relay, channels: 3, frameSize: 480, expectedFrames: 50)
    backend.stop()
    thread.stop()
    #expect(output[0] == output[2], "passthrough leaves the mic identical to the raw copy")
    #expect(abs(rmsDecibels(output[0][...]) - -15.05) < 0.1)
    #expect(abs(rmsDecibels(output[1][...]) - -9.03) < 0.1)
    #expect(thread.levels.levels.system != nil)
  }

  @Test func inPersonSingleLaneMetersAsMic() throws {
    let lanes: [AudioLane] = [.mixed]
    let backend = SyntheticCaptureBackend(lanes: lanes, tone: [.mixed: 500], seconds: 0.3)
    let sink = LaneFrameSink(lanes: lanes)
    let relay = FrameRelay(channels: 1, frameSize: 480, capacityFrames: 100)
    let thread = ProcessingThread(
      sink: sink, relay: relay, configuration: .init(lanes: lanes, echoCanceller: nil))
    thread.start()
    try backend.start(lanes: lanes, inputDeviceUID: nil, sink: sink)
    let output = drain(relay, channels: 1, frameSize: 480, expectedFrames: 30)
    backend.stop()
    thread.stop()
    #expect(output[0].count == 14_400)
    let levels = thread.levels.levels
    #expect(levels.system == nil)
    #expect(abs(levels.mic.rms - -9.03) < 0.2)
    #expect(thread.systemPeak == 0)
  }

  @Test func aStalledWriterIsCountedNotWaitedFor() throws {
    let lanes: [AudioLane] = [.mixed]
    let backend = SyntheticCaptureBackend(lanes: lanes, tone: [.mixed: 500], seconds: 1)
    let sink = LaneFrameSink(lanes: lanes)
    // Room for 20 frames only and nobody draining: 80 of 100 frames are refused.
    let relay = FrameRelay(channels: 1, frameSize: 480, capacityFrames: 20)
    let thread = ProcessingThread(
      sink: sink, relay: relay, configuration: .init(lanes: lanes, echoCanceller: nil))
    thread.start()
    try backend.start(lanes: lanes, inputDeviceUID: nil, sink: sink)
    var waited = 0
    while backend.framesDelivered < 48_000, waited < 5_000 {
      Thread.sleep(forTimeInterval: 0.001)
      waited += 1
    }
    backend.stop()
    thread.stop()
    // The ring rounds up to a power of two, so the exact headroom is derived.
    let headroom = relay.rings[0].capacity / 480
    #expect(headroom >= 20)
    #expect(thread.framesProcessed == 100)
    #expect(relay.droppedFrames == [100 - headroom])
    #expect(relay.availableFrames == headroom)
  }

  @Test func syntheticEchoIsADelayedCopy() throws {
    let lanes: [AudioLane] = [.mic, .system]
    let backend = SyntheticCaptureBackend(
      signals: [
        .mic: SyntheticLane(
          frequency: 0, amplitude: 0, echo: .init(of: .system, delay: 0.060, gain: 0.5)),
        .system: SyntheticLane(frequency: 1_000, amplitude: 0.5),
      ], seconds: 0.5, callbackFrames: 333)
    let sink = LaneFrameSink(lanes: lanes)
    let relay = FrameRelay(channels: 2, frameSize: 480, capacityFrames: 100)
    let thread = ProcessingThread(
      sink: sink, relay: relay, configuration: .init(lanes: lanes, echoCanceller: nil))
    thread.start()
    try backend.start(lanes: lanes, inputDeviceUID: nil, sink: sink)
    let output = drain(relay, channels: 2, frameSize: 480, expectedFrames: 50)
    backend.stop()
    thread.stop()
    let delay = 2_880
    #expect(output[0][..<delay].allSatisfy { $0 == 0 }, "silent until the delay elapsed")
    var maxError: Float = 0
    for index in delay..<24_000 {
      maxError = max(maxError, abs(output[0][index] - 0.5 * output[1][index - delay]))
    }
    #expect(maxError < 1e-4)
  }
}
