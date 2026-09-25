import Foundation
import StenoCore
import Synchronization
import Testing

@testable import StenoAudio

/// The producer protocol the IOProc and the synthetic backend follow, and the
/// relay between the processing and writer threads: a block that does not
/// fit in every ring is refused for every lane, so the lanes never drift
/// apart, and the refusal is counted on every lane.
@Suite struct LaneFrameSinkTests {
  private final class Counter: Sendable {
    let value = Atomic<Int>(0)
  }

  @Test func aCallbackThatFitsOneLaneButNotTheOtherIsRefusedForBoth() {
    // 1 000 samples of headroom round up to 1 024 per ring.
    let sink = LaneFrameSink(lanes: [.mic, .system], sampleRate: 1_000, ringSeconds: 1)
    let block = [Float](repeating: 0.5, count: 400)
    block.withUnsafeBufferPointer { source in
      for _ in 0..<2 {
        #expect(sink.beginCallback(frameCount: 400))
        sink.write(lane: 0, from: source.baseAddress!)
        sink.write(lane: 1, from: source.baseAddress!)
        sink.endCallback()
      }
      // Drain the mic ring alone; the system ring still holds 800 of 1 024.
      var scratch = [Float](repeating: 0, count: 800)
      scratch.withUnsafeMutableBufferPointer {
        #expect(sink.ring(0).read(into: $0.baseAddress!, count: 800))
      }
      #expect(sink.ring(0).hasRoom(for: 400))
      #expect(!sink.ring(1).hasRoom(for: 400))
      #expect(!sink.beginCallback(frameCount: 400), "refused as a whole")
    }
    #expect(sink.droppedSamples == [.mic: 400, .system: 400], "counted on every lane")
    #expect(sink.ring(0).availableToRead == 0, "nothing landed on the lane that had room")
    #expect(sink.ring(1).availableToRead == 800)
    #expect(sink.availableToRead == 0, "the consumer sees the minimum over lanes")
  }

  @Test func silenceKeepsTheLaneAlignedAndDeviceLossFiresOnce() {
    let lost = Counter()
    let sink = LaneFrameSink(lanes: [.mic, .system], sampleRate: 1_000, ringSeconds: 1) {
      lost.value.wrappingAdd(1, ordering: .relaxed)
    }
    let block: [Float] = [1, 2, 3]
    block.withUnsafeBufferPointer {
      #expect(sink.beginCallback(frameCount: 3))
      sink.write(lane: 0, from: $0.baseAddress!)
      sink.writeSilence(lane: 1)
      sink.endCallback()
    }
    #expect(sink.wake.wait(timeout: .now()) == .success, "one wake per callback")
    #expect(sink.wake.wait(timeout: .now()) == .timedOut)
    #expect(sink.availableToRead == 3, "the silent lane advanced with the other")
    var out = [Float](repeating: 9, count: 3)
    out.withUnsafeMutableBufferPointer { _ = sink.ring(1).read(into: $0.baseAddress!, count: 3) }
    #expect(out == [0, 0, 0])
    out.withUnsafeMutableBufferPointer { _ = sink.ring(0).read(into: $0.baseAddress!, count: 3) }
    #expect(out == [1, 2, 3])

    sink.reportDeviceLost()
    sink.reportDeviceLost()
    #expect(lost.value.load(ordering: .relaxed) == 1, "later reports are ignored")

    block.withUnsafeBufferPointer {
      _ = sink.beginCallback(frameCount: 3)
      sink.write(lane: 0, from: $0.baseAddress!)
      sink.write(lane: 1, from: $0.baseAddress!)
      sink.endCallback()
    }
    sink.clear()
    #expect(sink.availableToRead == 0)
    #expect(sink.droppedSamples.isEmpty)
  }

  @Test func theRelayRefusesAndCountsAFrameOnEveryChannel() {
    // Two frames of four samples per channel: the ring is exactly eight.
    let relay = FrameRelay(channels: 3, frameSize: 4, capacityFrames: 2)
    let frame: [Float] = [1, 2, 3, 4]
    frame.withUnsafeBufferPointer { source in
      for _ in 0..<2 {
        #expect(relay.beginFrame())
        for channel in 0..<3 { relay.write(channel: channel, from: source.baseAddress!) }
        relay.endFrame()
      }
      #expect(relay.availableFrames == 2)
      #expect(!relay.beginFrame(), "full: refused for every channel")
    }
    #expect(relay.droppedFrames == [1, 1, 1])
    var out = [Float](repeating: 0, count: 4)
    out.withUnsafeMutableBufferPointer { #expect(relay.read(channel: 2, into: $0.baseAddress!)) }
    #expect(out == frame)
    #expect(relay.availableFrames == 1, "whole frames every channel has: the minimum")
    #expect(relay.wake.wait(timeout: .now()) == .success)
    #expect(relay.wake.wait(timeout: .now()) == .success)
    #expect(relay.wake.wait(timeout: .now()) == .timedOut, "no wake for the refused frame")
  }
}
