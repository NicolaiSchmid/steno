import Foundation
import StenoCore
import Synchronization

/// The ring writer handed to the backend: `LaneRings` with lane names, a
/// wake per callback and the device-lost signal. Runs on the HAL's IOProc
/// thread (or the synthetic backend's producer thread).
///
/// Producer protocol (real-time safe, one producer at a time):
/// `beginCallback(frameCount:)` reserves the whole callback on every ring
/// (or counts it as dropped for every lane and returns false), then one
/// `write`/`writeMixed`/`writeSilence` per lane, then `endCallback()`.
/// Nothing in that path allocates or locks.
public final class LaneFrameSink: @unchecked Sendable {
  public let lanes: [AudioLane]
  let rings: LaneRings
  private let deviceLost = Atomic<Bool>(false)
  private let deviceLostHandler: @Sendable () -> Void
  /// Producer-only scratch for the callback in flight.
  private var pendingFrames = 0

  /// `ringSeconds` of headroom per lane absorbs a stalled consumer.
  public init(
    lanes: [AudioLane], sampleRate: Double = StenoAudio.sampleRate, ringSeconds: Double = 2,
    onDeviceLost: @escaping @Sendable () -> Void = {}
  ) {
    self.lanes = lanes
    self.rings = LaneRings(count: lanes.count, capacity: Int(sampleRate * ringSeconds))
    self.deviceLostHandler = onDeviceLost
  }

  // MARK: Producer (real-time)

  @inline(__always)
  public func beginCallback(frameCount: Int) -> Bool {
    guard rings.reserve(frameCount) else { return false }
    pendingFrames = frameCount
    return true
  }

  @inline(__always)
  public func write(lane: Int, from source: UnsafePointer<Float>, stride: Int = 1) {
    rings.write(lane, from: source, count: pendingFrames, stride: stride)
  }

  /// Two channels folded to one lane; `rightStride` defaults to `stride`.
  @inline(__always)
  public func writeMixed(
    lane: Int, left: UnsafePointer<Float>, right: UnsafePointer<Float>, stride: Int = 1,
    rightStride: Int? = nil
  ) {
    rings.writeMixed(
      lane, left: left, right: right, count: pendingFrames, stride: stride,
      rightStride: rightStride)
  }

  @inline(__always)
  public func writeSilence(lane: Int) {
    rings.writeZeros(lane, count: pendingFrames)
  }

  @inline(__always)
  public func endCallback() {
    rings.commit()
  }

  // MARK: Backend (any thread)

  /// The backend's device-change or `DeviceIsAlive` listener calls this; the
  /// first call runs the handler, later calls are ignored.
  public func reportDeviceLost() {
    let (exchanged, _) = deviceLost.compareExchange(
      expected: false, desired: true, ordering: .acquiringAndReleasing)
    if exchanged { deviceLostHandler() }
  }

  // MARK: Consumer (processing thread)

  /// Signalled once per completed callback.
  var wake: DispatchSemaphore { rings.wake }

  func ring(_ lane: Int) -> LaneRingBuffer { rings[lane] }

  /// Samples every lane has queued right now (the minimum over lanes).
  var availableToRead: Int { rings.availableToRead }

  /// Ring overruns per lane, in samples.
  public var droppedSamples: [AudioLane: Int] {
    var result: [AudioLane: Int] = [:]
    for (lane, dropped) in zip(lanes, rings.droppedSamples) where dropped > 0 {
      result[lane] = dropped
    }
    return result
  }

  /// Zeroes every ring so a restart never replays stale frames. Only while
  /// no producer runs.
  func clear() {
    rings.clear()
  }
}
