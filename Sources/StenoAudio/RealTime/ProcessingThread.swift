import Foundation
import StenoCore
import Synchronization

/// Drains the sink's rings in 10 ms frames on a dedicated
/// `.userInteractive` thread: echo cancellation on the mic lane with the
/// system lane of the same frame as far-end (optionally delayed by the
/// device latency through a preallocated line), metering, and the hand-off
/// to the writer through `FrameRelay`. Every buffer is allocated in `init`;
/// the loop allocates nothing, takes no locks and never blocks on the
/// writer.
final class ProcessingThread: @unchecked Sendable {
  struct Configuration {
    var lanes: [AudioLane]
    var frameSize: Int = StenoAudio.frameSize
    var echoCanceller: (any EchoCanceller)?
    /// Frames the far-end is delayed by before cancellation (0: none).
    var farEndDelayFrames: Int = 0
    var keepRawMic: Bool = false
    /// Frames per level publish: 10 frames is 100 ms, 10 Hz.
    var framesPerLevel: Int = 10
  }

  let configuration: Configuration
  let levels: LevelSlot
  private let sink: LaneFrameSink
  private let relay: FrameRelay
  private let micIndex: Int?
  private let systemIndex: Int?
  private let laneBuffers: [UnsafeMutablePointer<Float>]
  private let processedMic: UnsafeMutablePointer<Float>
  private let rawMic: UnsafeMutablePointer<Float>
  private let delayedFar: UnsafeMutablePointer<Float>
  private let delayLine: LaneRingBuffer?
  private var meters: [LevelMeter]
  private var framesSincePublish = 0
  private let framesProcessedCount = Atomic<Int>(0)
  private let systemPeakBits = Atomic<UInt32>(0)
  private let stopRequested = Atomic<Bool>(false)
  private let finished = DispatchSemaphore(value: 0)
  private var thread: Thread?
  private let useEchoCancellation: Bool

  init(sink: LaneFrameSink, relay: FrameRelay, configuration: Configuration) {
    self.sink = sink
    self.relay = relay
    self.configuration = configuration
    let frameSize = configuration.frameSize
    micIndex = configuration.lanes.firstIndex(of: .mic)
    systemIndex = configuration.lanes.firstIndex(of: .system)
    laneBuffers = configuration.lanes.map { _ in
      let pointer = UnsafeMutablePointer<Float>.allocate(capacity: frameSize)
      pointer.initialize(repeating: 0, count: frameSize)
      return pointer
    }
    processedMic = .allocate(capacity: frameSize)
    processedMic.initialize(repeating: 0, count: frameSize)
    rawMic = .allocate(capacity: frameSize)
    rawMic.initialize(repeating: 0, count: frameSize)
    delayedFar = .allocate(capacity: frameSize)
    delayedFar.initialize(repeating: 0, count: frameSize)
    useEchoCancellation =
      configuration.echoCanceller != nil && micIndex != nil && systemIndex != nil
    if useEchoCancellation, configuration.farEndDelayFrames > 0 {
      let line = LaneRingBuffer(capacity: configuration.farEndDelayFrames + frameSize)
      line.writeZeros(count: configuration.farEndDelayFrames)
      delayLine = line
    } else {
      delayLine = nil
    }
    meters = configuration.lanes.map { _ in LevelMeter() }
    levels = LevelSlot(hasSystem: systemIndex != nil)
  }

  deinit {
    for buffer in laneBuffers { buffer.deallocate() }
    processedMic.deallocate()
    rawMic.deallocate()
    delayedFar.deallocate()
  }

  /// Frames handed to the relay or refused by it.
  var framesProcessed: Int { framesProcessedCount.load(ordering: .relaxed) }

  /// The loudest system-lane sample so far, linear.
  var systemPeak: Float { Float(bitPattern: systemPeakBits.load(ordering: .relaxed)) }

  func start() {
    let thread = Thread { [self] in run() }
    thread.name = "uno.schmid.steno.audio.processing"
    thread.qualityOfService = .userInteractive
    self.thread = thread
    thread.start()
  }

  /// Asks the loop to stop, waits for it to drain every whole frame left in
  /// the rings and exit.
  func stop() {
    guard thread != nil else { return }
    stopRequested.store(true, ordering: .releasing)
    sink.wake.signal()
    finished.wait()
    thread = nil
  }

  private func run() {
    while !stopRequested.load(ordering: .acquiring) {
      _ = sink.wake.wait(timeout: .now() + .milliseconds(20))
      drain()
    }
    drain()
    finished.signal()
  }

  @inline(__always)
  private func drain() {
    let frameSize = configuration.frameSize
    while sink.availableToRead >= frameSize {
      var index = 0
      while index < laneBuffers.count {
        sink.ring(index).read(into: laneBuffers[index], count: frameSize)
        index += 1
      }
      processFrame()
    }
  }

  @inline(__always)
  private func processFrame() {
    let frameSize = configuration.frameSize
    var micPointer: UnsafeMutablePointer<Float>?
    if let micIndex { micPointer = laneBuffers[micIndex] }

    if useEchoCancellation, let micIndex, let systemIndex,
      let canceller = configuration.echoCanceller
    {
      let mic = laneBuffers[micIndex]
      let system = laneBuffers[systemIndex]
      if configuration.keepRawMic {
        rawMic.update(from: mic, count: frameSize)
      }
      var farEnd = UnsafeBufferPointer<Float>(start: system, count: frameSize)
      if let delayLine {
        delayLine.write(system, count: frameSize)
        delayLine.read(into: delayedFar, count: frameSize)
        farEnd = UnsafeBufferPointer(start: delayedFar, count: frameSize)
      }
      canceller.process(
        nearEnd: UnsafeBufferPointer(start: mic, count: frameSize),
        farEnd: farEnd,
        out: UnsafeMutableBufferPointer(start: processedMic, count: frameSize))
      micPointer = processedMic
    } else if configuration.keepRawMic, let micIndex {
      rawMic.update(from: laneBuffers[micIndex], count: frameSize)
    }

    // Metering on what is written.
    var index = 0
    while index < laneBuffers.count {
      let source = (index == micIndex ? micPointer : nil) ?? laneBuffers[index]
      meters[index].accumulate(source, count: frameSize)
      index += 1
    }
    if let systemIndex {
      let peak = meters[systemIndex].linearPeak
      if peak > systemPeak { systemPeakBits.store(peak.bitPattern, ordering: .relaxed) }
    }
    framesSincePublish += 1
    if framesSincePublish >= configuration.framesPerLevel {
      framesSincePublish = 0
      // In person the single `.mixed` lane is what the meter calls "mic".
      let micLevel = meters[micIndex ?? 0].current
      let systemLevel = systemIndex.map { meters[$0].current }
      levels.publish(mic: micLevel, system: systemLevel)
      index = 0
      while index < meters.count {
        _ = meters[index].flush()
        index += 1
      }
    }

    // Hand-off; refused frames are counted by the relay.
    framesProcessedCount.wrappingAdd(1, ordering: .relaxed)
    guard relay.beginFrame() else { return }
    index = 0
    while index < laneBuffers.count {
      let source = (index == micIndex ? micPointer : nil) ?? laneBuffers[index]
      relay.write(channel: index, from: source)
      index += 1
    }
    if configuration.keepRawMic, micIndex != nil {
      relay.write(channel: laneBuffers.count, from: rawMic)
    }
    relay.endFrame()
  }
}
