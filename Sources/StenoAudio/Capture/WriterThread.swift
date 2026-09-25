import Foundation
import StenoCore
import Synchronization

/// The serial writer: drains `FrameRelay` on its own thread, hands each frame
/// to `RecordingWriter`, and republishes the processing thread's levels
/// whenever their generation changed (so the 10 Hz level stream costs the
/// processing thread nothing). A write error is kept, reported once and
/// stops further writes; the loop keeps draining so the relay never fills.
final class WriterThread: @unchecked Sendable {
  private let relay: FrameRelay
  private let writer: RecordingWriter
  private let levels: LevelSlot
  private let laneCount: Int
  private let hasRawMic: Bool
  private let frameSize: Int
  private let buffers: [UnsafeMutablePointer<Float>]
  private let onLevels: @Sendable (LaneLevels) -> Void
  private let onError: @Sendable (any Error) -> Void
  private let stopRequested = Atomic<Bool>(false)
  private let finished = DispatchSemaphore(value: 0)
  private let framesWrittenCount = Atomic<Int>(0)
  private let failed = Atomic<Bool>(false)
  private var lastGeneration = 0
  private var thread: Thread?

  init(
    relay: FrameRelay, writer: RecordingWriter, levels: LevelSlot, laneCount: Int, hasRawMic: Bool,
    onLevels: @escaping @Sendable (LaneLevels) -> Void,
    onError: @escaping @Sendable (any Error) -> Void
  ) {
    self.relay = relay
    self.writer = writer
    self.levels = levels
    self.laneCount = laneCount
    self.hasRawMic = hasRawMic
    self.frameSize = relay.frameSize
    self.buffers = (0..<relay.channels).map { _ in
      let pointer = UnsafeMutablePointer<Float>.allocate(capacity: relay.frameSize)
      pointer.initialize(repeating: 0, count: relay.frameSize)
      return pointer
    }
    self.onLevels = onLevels
    self.onError = onError
  }

  deinit {
    for buffer in buffers { buffer.deallocate() }
  }

  var framesWritten: Int { framesWrittenCount.load(ordering: .relaxed) }
  var hasFailed: Bool { failed.load(ordering: .acquiring) }

  func start() {
    let thread = Thread { [self] in run() }
    thread.name = "uno.schmid.steno.audio.writer"
    thread.qualityOfService = .userInitiated
    self.thread = thread
    thread.start()
  }

  /// Drains everything left in the relay, then returns.
  func stop() {
    guard thread != nil else { return }
    stopRequested.store(true, ordering: .releasing)
    relay.wake.signal()
    finished.wait()
    thread = nil
  }

  private func run() {
    while !stopRequested.load(ordering: .acquiring) {
      _ = relay.wake.wait(timeout: .now() + .milliseconds(50))
      drain()
      publishLevelsIfChanged()
    }
    drain()
    publishLevelsIfChanged()
    finished.signal()
  }

  private func drain() {
    while relay.availableFrames > 0 {
      var channel = 0
      while channel < buffers.count {
        _ = relay.read(channel: channel, into: buffers[channel])
        channel += 1
      }
      guard !hasFailed else { continue }
      let frames = LaneFrames(
        frameCount: frameSize,
        lanes: (0..<laneCount).map { UnsafePointer(buffers[$0]) },
        rawMic: hasRawMic ? UnsafePointer(buffers[laneCount]) : nil)
      do {
        try writer.write(frames)
        framesWrittenCount.wrappingAdd(1, ordering: .relaxed)
      } catch {
        failed.store(true, ordering: .releasing)
        onError(error)
      }
    }
  }

  private func publishLevelsIfChanged() {
    let generation = levels.currentGeneration
    guard generation != lastGeneration else { return }
    lastGeneration = generation
    onLevels(levels.levels)
  }
}
