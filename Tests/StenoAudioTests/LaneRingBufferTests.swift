import Foundation
import Testing

@testable import StenoAudio

/// Run these under `swift test --sanitize=thread --filter LaneRingBufferTests`
/// on a developer machine as well; CI runs them plain.
@Suite struct LaneRingBufferTests {
  @Test func capacityRoundsUpToAPowerOfTwo() {
    #expect(LaneRingBuffer(capacity: 1000).capacity == 1024)
    #expect(LaneRingBuffer(capacity: 1024).capacity == 1024)
    #expect(LaneRingBuffer(capacity: 1).capacity == 2)
  }

  @Test func writeThenReadRoundTripsAcrossTheWrapPoint() {
    let ring = LaneRingBuffer(capacity: 8)
    var out = [Float](repeating: 0, count: 8)
    for round in 0..<5 {
      let input = (0..<5).map { Float(round * 10 + $0) }
      input.withUnsafeBufferPointer { #expect(ring.write($0.baseAddress!, count: 5)) }
      #expect(ring.availableToRead == 5)
      out.withUnsafeMutableBufferPointer { #expect(ring.read(into: $0.baseAddress!, count: 5)) }
      #expect(Array(out.prefix(5)) == input)
    }
    #expect(ring.droppedSamples == 0)
  }

  @Test func stridedWriteDeinterleaves() {
    let ring = LaneRingBuffer(capacity: 8)
    let interleaved: [Float] = [1, 100, 2, 200, 3, 300]
    interleaved.withUnsafeBufferPointer {
      #expect(ring.write($0.baseAddress! + 1, count: 3, stride: 2))
    }
    var out = [Float](repeating: 0, count: 3)
    out.withUnsafeMutableBufferPointer { #expect(ring.read(into: $0.baseAddress!, count: 3)) }
    #expect(out == [100, 200, 300])
  }

  @Test func mixedWriteAveragesTwoChannels() {
    let ring = LaneRingBuffer(capacity: 8)
    let left: [Float] = [1, 1, 1]
    let right: [Float] = [0, 0.5, -1]
    left.withUnsafeBufferPointer { l in
      right.withUnsafeBufferPointer { r in
        #expect(ring.writeMixed(l.baseAddress!, r.baseAddress!, count: 3))
      }
    }
    var out = [Float](repeating: 0, count: 3)
    out.withUnsafeMutableBufferPointer { #expect(ring.read(into: $0.baseAddress!, count: 3)) }
    #expect(out == [0.5, 0.75, 0])
  }

  @Test func fullRingRefusesTheWholeBlockAndCountsIt() {
    let ring = LaneRingBuffer(capacity: 8)
    let block = [Float](repeating: 1, count: 6)
    block.withUnsafeBufferPointer {
      #expect(ring.write($0.baseAddress!, count: 6))
      #expect(!ring.write($0.baseAddress!, count: 6))
      #expect(!ring.hasRoom(for: 3))
      #expect(ring.hasRoom(for: 2))
    }
    #expect(ring.droppedSamples == 6)
    #expect(ring.availableToRead == 6, "a refused write changes nothing")
    var out = [Float](repeating: 0, count: 8)
    out.withUnsafeMutableBufferPointer {
      #expect(!ring.read(into: $0.baseAddress!, count: 7), "short reads are refused as a whole")
      #expect(ring.read(into: $0.baseAddress!, count: 6))
    }
    #expect(ring.skip(10) == 0)
    ring.writeZeros(count: 4)
    #expect(ring.skip(10) == 4)
  }

  @Test func clearZeroesStorageAndResetsIndices() {
    let ring = LaneRingBuffer(capacity: 4)
    let block: [Float] = [1, 2, 3]
    block.withUnsafeBufferPointer { _ = ring.write($0.baseAddress!, count: 3) }
    block.withUnsafeBufferPointer { _ = ring.write($0.baseAddress!, count: 3) }
    ring.clear()
    #expect(ring.availableToRead == 0)
    #expect(ring.droppedSamples == 0)
    ring.writeZeros(count: 4)
    var out = [Float](repeating: 9, count: 4)
    out.withUnsafeMutableBufferPointer { _ = ring.read(into: $0.baseAddress!, count: 4) }
    #expect(out == [0, 0, 0, 0])
  }

  /// A producer thread writes 480-sample blocks as fast as it can while a
  /// consumer thread drains; every sample is either read in order or counted
  /// as dropped, and nothing is duplicated or lost.
  @Test func concurrentProducerAndConsumerAccountForEverySample() {
    let ring = LaneRingBuffer(capacity: 4096)
    let block = 480
    let blocks = 2_000
    let consumed = Consumed()

    let producer = Thread {
      var buffer = [Float](repeating: 0, count: block)
      for index in 0..<blocks {
        for offset in 0..<block { buffer[offset] = Float(index) }
        buffer.withUnsafeBufferPointer { _ = ring.write($0.baseAddress!, count: block) }
      }
      consumed.producerDone()
    }
    let consumer = Thread {
      var buffer = [Float](repeating: 0, count: block)
      var lastBlock = -1
      var ordered = true
      var count = 0
      while true {
        let got = buffer.withUnsafeMutableBufferPointer {
          ring.read(into: $0.baseAddress!, count: block)
        }
        if got {
          let value = Int(buffer[0])
          if buffer.contains(where: { Int($0) != value }) || value <= lastBlock { ordered = false }
          lastBlock = value
          count += 1
        } else if consumed.isProducerDone {
          break
        } else {
          Thread.sleep(forTimeInterval: 0.0002)
        }
      }
      consumed.finish(blocks: count, ordered: ordered)
    }
    producer.start()
    consumer.start()
    #expect(consumed.wait(seconds: 30))
    let dropped = ring.droppedSamples / block
    #expect(consumed.blocks + dropped == blocks, "read \(consumed.blocks) + dropped \(dropped)")
    #expect(consumed.ordered)
  }

  private final class Consumed: @unchecked Sendable {
    private let lock = NSLock()
    private let done = DispatchSemaphore(value: 0)
    private var _producerDone = false
    private(set) var blocks = 0
    private(set) var ordered = false

    var isProducerDone: Bool {
      lock.lock()
      defer { lock.unlock() }
      return _producerDone
    }

    func producerDone() {
      lock.lock()
      _producerDone = true
      lock.unlock()
    }

    func finish(blocks: Int, ordered: Bool) {
      lock.lock()
      self.blocks = blocks
      self.ordered = ordered
      lock.unlock()
      done.signal()
    }

    func wait(seconds: Double) -> Bool {
      done.wait(timeout: .now() + seconds) == .success
    }
  }
}
