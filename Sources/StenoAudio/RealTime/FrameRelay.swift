import Foundation
import Synchronization

/// The bounded hand-off from the processing thread to the writer thread:
/// one `LaneRingBuffer` per channel (the lanes, plus the raw mic when kept),
/// a semaphore per committed frame and drop accounting. The processing
/// thread never blocks on it: when a frame does not fit, every channel of
/// that frame is refused and counted.
final class FrameRelay: @unchecked Sendable {
  let channels: Int
  let frameSize: Int
  let rings: [LaneRingBuffer]
  let wake = DispatchSemaphore(value: 0)

  /// `capacityFrames` whole frames of headroom per channel.
  init(channels: Int, frameSize: Int, capacityFrames: Int) {
    self.channels = channels
    self.frameSize = frameSize
    self.rings = (0..<channels).map { _ in
      LaneRingBuffer(capacity: frameSize * capacityFrames)
    }
  }

  // MARK: Producer (processing thread)

  @inline(__always)
  func beginFrame() -> Bool {
    var fits = true
    var index = 0
    while index < rings.count {
      if !rings[index].hasRoom(for: frameSize) { fits = false }
      index += 1
    }
    if !fits {
      index = 0
      while index < rings.count {
        rings[index].recordDrop(frameSize)
        index += 1
      }
    }
    return fits
  }

  @inline(__always)
  func write(channel: Int, from source: UnsafePointer<Float>) {
    rings[channel].write(source, count: frameSize)
  }

  @inline(__always)
  func endFrame() {
    wake.signal()
  }

  // MARK: Consumer (writer thread)

  /// Whole frames every channel has queued. A loop, not `map`, for the same
  /// reason as `LaneFrameSink.availableToRead`.
  var availableFrames: Int {
    var minimum = Int.max
    var index = 0
    while index < rings.count {
      let available = rings[index].availableToRead
      if available < minimum { minimum = available }
      index += 1
    }
    return minimum == Int.max ? 0 : minimum / frameSize
  }

  @inline(__always)
  func read(channel: Int, into destination: UnsafeMutablePointer<Float>) -> Bool {
    rings[channel].read(into: destination, count: frameSize)
  }

  /// Frames refused per channel.
  var droppedFrames: [Int] {
    rings.map { $0.droppedSamples / frameSize }
  }
}
