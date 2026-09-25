import Foundation

/// One ring per channel plus the invariant that keeps channels aligned: a
/// block is reserved for every ring or refused, and counted as dropped, for
/// every ring. The producer (IOProc, synthetic backend, processing thread)
/// calls `reserve`, one `write`/`writeMixed`/`writeZeros` per channel, then
/// `commit`, which wakes the consumer once. Nothing on that path allocates or
/// locks; the loops are `while`, not `map`, because a debug build allocates
/// for `map`.
///
/// `LaneFrameSink` (IOProc → processing thread) and `FrameRelay` (processing
/// thread → writer thread) are this type with lane names and a fixed frame
/// size respectively; the reservation logic lives here once.
final class LaneRings: @unchecked Sendable {
  private let rings: [LaneRingBuffer]
  /// Signalled once per committed block; the consumer waits on it.
  let wake = DispatchSemaphore(value: 0)

  /// `count` rings of `capacity` samples each (rounded up to a power of two).
  init(count: Int, capacity: Int) {
    rings = (0..<count).map { _ in LaneRingBuffer(capacity: capacity) }
  }

  var count: Int { rings.count }

  subscript(_ channel: Int) -> LaneRingBuffer { rings[channel] }

  // MARK: Producer (real-time)

  /// Whether every ring can take `count` more samples right now.
  @inline(__always)
  func hasRoom(for count: Int) -> Bool {
    var index = 0
    while index < rings.count {
      if !rings[index].hasRoom(for: count) { return false }
      index += 1
    }
    return true
  }

  /// All or nothing: true when every ring has room for `count` samples;
  /// otherwise the block is counted as dropped on every ring and nothing is
  /// written.
  @inline(__always)
  func reserve(_ count: Int) -> Bool {
    if hasRoom(for: count) { return true }
    var index = 0
    while index < rings.count {
      rings[index].recordDrop(count)
      index += 1
    }
    return false
  }

  @inline(__always)
  func write(_ channel: Int, from source: UnsafePointer<Float>, count: Int, stride: Int = 1) {
    rings[channel].write(source, count: count, stride: stride)
  }

  @inline(__always)
  func writeMixed(
    _ channel: Int, left: UnsafePointer<Float>, right: UnsafePointer<Float>, count: Int,
    stride: Int = 1, rightStride: Int? = nil
  ) {
    rings[channel].writeMixed(left, right, count: count, stride: stride, rightStride: rightStride)
  }

  @inline(__always)
  func writeZeros(_ channel: Int, count: Int) {
    rings[channel].writeZeros(count: count)
  }

  /// Wakes the consumer once for the block just written.
  @inline(__always)
  func commit() {
    wake.signal()
  }

  // MARK: Consumer

  /// Samples every channel has queued right now: the minimum over rings.
  var availableToRead: Int {
    var minimum = Int.max
    var index = 0
    while index < rings.count {
      let available = rings[index].availableToRead
      if available < minimum { minimum = available }
      index += 1
    }
    return minimum == Int.max ? 0 : minimum
  }

  @inline(__always)
  @discardableResult
  func read(_ channel: Int, into destination: UnsafeMutablePointer<Float>, count: Int) -> Bool {
    rings[channel].read(into: destination, count: count)
  }

  /// Refused samples per channel.
  var droppedSamples: [Int] {
    rings.map(\.droppedSamples)
  }

  /// Zeroes every ring so a restart never replays stale frames. Only while
  /// no producer runs.
  func clear() {
    for ring in rings { ring.clear() }
  }
}
