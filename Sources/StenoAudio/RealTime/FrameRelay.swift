import Foundation

/// The bounded hand-off from the processing thread to the writer thread:
/// `LaneRings` with one channel per written channel (the lanes, plus the
/// raw mic when kept) and a fixed frame size. The processing thread never
/// blocks on it: when a frame does not fit, every channel of that frame is
/// refused and counted.
final class FrameRelay: @unchecked Sendable {
  let channels: Int
  let frameSize: Int
  let rings: LaneRings

  /// `capacityFrames` whole frames of headroom per channel.
  init(channels: Int, frameSize: Int, capacityFrames: Int) {
    self.channels = channels
    self.frameSize = frameSize
    self.rings = LaneRings(count: channels, capacity: frameSize * capacityFrames)
  }

  /// Signalled once per committed frame; the writer thread waits on it.
  var wake: DispatchSemaphore { rings.wake }

  // MARK: Producer (processing thread)

  @inline(__always)
  func beginFrame() -> Bool {
    rings.reserve(frameSize)
  }

  @inline(__always)
  func write(channel: Int, from source: UnsafePointer<Float>) {
    rings.write(channel, from: source, count: frameSize)
  }

  @inline(__always)
  func endFrame() {
    rings.commit()
  }

  // MARK: Consumer (writer thread)

  /// Whole frames every channel has queued.
  var availableFrames: Int {
    rings.availableToRead / frameSize
  }

  @inline(__always)
  func read(channel: Int, into destination: UnsafeMutablePointer<Float>) -> Bool {
    rings.read(channel, into: destination, count: frameSize)
  }

  /// Frames refused per channel.
  var droppedFrames: [Int] {
    rings.droppedSamples.map { $0 / frameSize }
  }
}
