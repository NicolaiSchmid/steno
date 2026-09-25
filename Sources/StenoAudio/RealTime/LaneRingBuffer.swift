import Synchronization

/// A single-producer, single-consumer ring of Float samples for one lane. The
/// IOProc writes, the processing thread reads. Indices are monotonically
/// increasing sample counts in `Atomic<Int>`; the producer publishes with a
/// releasing store, the consumer observes with an acquiring load. No locks, no
/// allocation after `init`.
///
/// A write that does not fit is refused as a whole (the caller keeps lanes
/// aligned by refusing every lane of that callback) and counted in
/// `droppedSamples`.
public final class LaneRingBuffer: @unchecked Sendable {
  /// Samples the ring holds; a power of two.
  public let capacity: Int
  private let mask: Int
  private let storage: UnsafeMutablePointer<Float>
  private let writeIndex = Atomic<Int>(0)
  private let readIndex = Atomic<Int>(0)
  private let dropped = Atomic<Int>(0)

  /// `capacity` is rounded up to a power of two, at least 2.
  public init(capacity: Int) {
    var size = 2
    while size < capacity { size <<= 1 }
    self.capacity = size
    self.mask = size - 1
    self.storage = .allocate(capacity: size)
    self.storage.initialize(repeating: 0, count: size)
  }

  deinit {
    storage.deallocate()
  }

  /// Samples the consumer may read right now.
  public var availableToRead: Int {
    writeIndex.load(ordering: .acquiring) - readIndex.load(ordering: .acquiring)
  }

  /// Samples the producer may write right now.
  public var availableToWrite: Int {
    capacity - availableToRead
  }

  /// Samples refused because the ring was full.
  public var droppedSamples: Int {
    dropped.load(ordering: .relaxed)
  }

  /// Producer side. Whether `count` samples fit right now.
  @inline(__always)
  public func hasRoom(for count: Int) -> Bool {
    availableToWrite >= count
  }

  /// Producer side. Records `count` refused samples without writing.
  @inline(__always)
  public func recordDrop(_ count: Int) {
    dropped.wrappingAdd(count, ordering: .relaxed)
  }

  /// Producer side. The write index when `count` more samples fit; nil, with
  /// the drop counted, when they do not.
  @inline(__always)
  private func reserve(_ count: Int) -> Int? {
    let write = writeIndex.load(ordering: .relaxed)
    let read = readIndex.load(ordering: .acquiring)
    guard capacity - (write - read) >= count else {
      dropped.wrappingAdd(count, ordering: .relaxed)
      return nil
    }
    return write
  }

  /// Producer side. Copies `count` samples read from `source` every `stride`
  /// floats (1 for a non-interleaved channel, the channel count for an
  /// interleaved buffer). Returns false and counts the drop when the samples
  /// do not fit.
  @discardableResult
  public func write(_ source: UnsafePointer<Float>, count: Int, stride: Int = 1) -> Bool {
    guard count > 0 else { return true }
    guard let write = reserve(count) else { return false }
    var position = write & mask
    var index = 0
    while index < count {
      storage[position] = source[index * stride]
      position = (position + 1) & mask
      index += 1
    }
    writeIndex.store(write + count, ordering: .releasing)
    return true
  }

  /// Producer side. Writes the average of two channels (a stereo tap folded
  /// to the mono system lane).
  @discardableResult
  public func writeMixed(
    _ left: UnsafePointer<Float>, _ right: UnsafePointer<Float>, count: Int, stride: Int = 1
  ) -> Bool {
    guard count > 0 else { return true }
    guard let write = reserve(count) else { return false }
    var position = write & mask
    var index = 0
    while index < count {
      storage[position] = (left[index * stride] + right[index * stride]) * 0.5
      position = (position + 1) & mask
      index += 1
    }
    writeIndex.store(write + count, ordering: .releasing)
    return true
  }

  /// Consumer side. Copies exactly `count` samples into `destination` or, when
  /// fewer are available, copies nothing and returns false.
  @discardableResult
  public func read(into destination: UnsafeMutablePointer<Float>, count: Int) -> Bool {
    guard count > 0 else { return true }
    let read = readIndex.load(ordering: .relaxed)
    let write = writeIndex.load(ordering: .acquiring)
    guard write - read >= count else { return false }
    var position = read & mask
    var index = 0
    while index < count {
      destination[index] = storage[position]
      position = (position + 1) & mask
      index += 1
    }
    readIndex.store(read + count, ordering: .releasing)
    return true
  }

  /// Consumer side, not real-time: takes everything queued as an array (the
  /// permission probe inspects what the tap delivered).
  public func drainAll() -> [Float] {
    let count = availableToRead
    guard count > 0 else { return [] }
    var samples = [Float](repeating: 0, count: count)
    samples.withUnsafeMutableBufferPointer { _ = read(into: $0.baseAddress!, count: count) }
    return samples
  }

  /// Empties the ring and zeroes its storage so a restart never replays stale
  /// frames. Only while no producer is running.
  public func clear() {
    storage.update(repeating: 0, count: capacity)
    readIndex.store(0, ordering: .sequentiallyConsistent)
    writeIndex.store(0, ordering: .sequentiallyConsistent)
    dropped.store(0, ordering: .sequentiallyConsistent)
  }
}
