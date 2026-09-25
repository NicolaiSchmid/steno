import Foundation
import StenoCore
import Synchronization

/// The HAL seam under `CaptureSession`: a backend delivers frames for every
/// lane into the `LaneFrameSink` from its own real-time context and reports
/// device loss. `LiveCaptureBackend` is the tap + aggregate + IOProc;
/// `SyntheticCaptureBackend` (Testing/) generates deterministic tones.
public protocol CaptureBackend: Sendable {
  /// Starts delivering `lanes` (in this order) at `StenoAudio.sampleRate`.
  /// `inputDeviceUID` nil selects the default input device. Throws a
  /// `CaptureError` when a device or the tap cannot be set up.
  func start(lanes: [AudioLane], inputDeviceUID: String?, sink: LaneFrameSink) throws
  /// Stops delivering; idempotent. No frame arrives after it returns.
  func stop()
}

/// The ring writer handed to the backend: one `LaneRingBuffer` per lane, a
/// semaphore that wakes the processing thread once per callback, drop
/// accounting and the device-lost signal.
///
/// Producer protocol (real-time safe, one producer at a time):
/// `beginCallback(frameCount:hostTime:)` checks every ring has room (or
/// counts the whole callback as dropped for every lane and returns false),
/// then one `write`/`writeMixed`/`writeSilence` per lane, then
/// `endCallback()`. Nothing in that path allocates or locks.
public final class LaneFrameSink: @unchecked Sendable {
  public let lanes: [AudioLane]
  public let sampleRate: Double
  let rings: [LaneRingBuffer]
  /// Signalled once per completed callback; the processing thread waits on it.
  let wake = DispatchSemaphore(value: 0)
  private let callbackCount = Atomic<Int>(0)
  private let lastHostTime = Atomic<UInt64>(0)
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
    self.sampleRate = sampleRate
    self.rings = lanes.map { _ in LaneRingBuffer(capacity: Int(sampleRate * ringSeconds)) }
    self.deviceLostHandler = onDeviceLost
  }

  public var laneCount: Int { lanes.count }

  /// Completed callbacks so far.
  public var callbacks: Int { callbackCount.load(ordering: .relaxed) }

  /// Host time of the most recent callback's first frame.
  public var latestHostTime: UInt64 { lastHostTime.load(ordering: .relaxed) }

  // MARK: Producer (real-time)

  @inline(__always)
  public func beginCallback(frameCount: Int, hostTime: UInt64) -> Bool {
    var fits = true
    var index = 0
    while index < rings.count {
      if !rings[index].hasRoom(for: frameCount) { fits = false }
      index += 1
    }
    if !fits {
      index = 0
      while index < rings.count {
        rings[index].recordDrop(frameCount)
        index += 1
      }
      return false
    }
    pendingFrames = frameCount
    lastHostTime.store(hostTime, ordering: .relaxed)
    return true
  }

  @inline(__always)
  public func write(lane: Int, from source: UnsafePointer<Float>, stride: Int = 1) {
    rings[lane].write(source, count: pendingFrames, stride: stride)
  }

  @inline(__always)
  public func writeMixed(
    lane: Int, left: UnsafePointer<Float>, right: UnsafePointer<Float>, stride: Int = 1
  ) {
    rings[lane].writeMixed(left, right, count: pendingFrames, stride: stride)
  }

  @inline(__always)
  public func writeSilence(lane: Int) {
    rings[lane].writeZeros(count: pendingFrames)
  }

  @inline(__always)
  public func endCallback() {
    callbackCount.wrappingAdd(1, ordering: .relaxed)
    wake.signal()
  }

  // MARK: Backend (any thread)

  /// The backend's device-change or `DeviceIsAlive` listener calls this; the
  /// first call runs the handler, later calls are ignored.
  public func reportDeviceLost() {
    let (exchanged, _) = deviceLost.compareExchange(
      expected: false, desired: true, ordering: .acquiringAndReleasing)
    if exchanged { deviceLostHandler() }
  }

  public var isDeviceLost: Bool { deviceLost.load(ordering: .acquiring) }

  // MARK: Consumer

  func ring(_ lane: Int) -> LaneRingBuffer { rings[lane] }

  /// Samples every lane has queued right now (the minimum over lanes).
  var availableToRead: Int {
    rings.map(\.availableToRead).min() ?? 0
  }

  /// Ring overruns per lane, in samples.
  public var droppedSamples: [AudioLane: Int] {
    var result: [AudioLane: Int] = [:]
    for (lane, ring) in zip(lanes, rings) where ring.droppedSamples > 0 {
      result[lane] = ring.droppedSamples
    }
    return result
  }

  /// Zeroes every ring so a restart never replays stale frames. Only while
  /// no producer runs.
  func clear() {
    for ring in rings { ring.clear() }
  }
}

extension LaneRingBuffer {
  /// Producer side. Writes `count` zeros (a buffer the HAL delivered without
  /// data keeps the lane aligned).
  @discardableResult
  public func writeZeros(count: Int) -> Bool {
    guard count > 0 else { return true }
    var zero: Float = 0
    return withUnsafePointer(to: &zero) { write($0, count: count, stride: 0) }
  }
}
