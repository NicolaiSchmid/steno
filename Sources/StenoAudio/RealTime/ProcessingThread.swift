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
///
/// The rings carry the device's rate. A device that does not run at
/// `StenoAudio.sampleRate` has every lane converted to it by a
/// `RateConverter` first, so everything below works at 48 kHz.
final class ProcessingThread: @unchecked Sendable {
  struct Configuration {
    var lanes: [AudioLane]
    var frameSize: Int = StenoAudio.frameSize
    /// The rate the rings carry, the device's; anything but
    /// `StenoAudio.sampleRate` is converted to it.
    var deviceRate: Double = StenoAudio.sampleRate
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
  /// What the rings deliver, one buffer per lane.
  private let laneBuffers: [UnsafeMutablePointer<Float>]
  /// Set when the device does not run at `StenoAudio.sampleRate`.
  private let conversion: Conversion?
  /// What is metered and written: `laneBuffers`, except that the mic lane
  /// points at the canceller's output while echo cancellation runs. The raw
  /// mic channel is then `laneBuffers[micIndex]`, untouched.
  private let outputs: [UnsafeMutablePointer<Float>]
  private let processedMic: UnsafeMutablePointer<Float>
  private let delayedFar: UnsafeMutablePointer<Float>
  private let delayLine: LaneRingBuffer?
  /// Set only when both a mic and a system lane exist.
  private let aec: (canceller: any EchoCanceller, mic: Int, system: Int)?
  private var meters: [LevelMeter]
  private var framesSincePublish = 0
  private let framesProcessedCount = Atomic<Int>(0)
  private let systemPeakBits = Atomic<UInt32>(0)
  private let stopRequested = Atomic<Bool>(false)
  private let finished = DispatchSemaphore(value: 0)
  private var thread: Thread?

  /// `levels` nil allocates a fresh slot; the session passes the previous
  /// thread's slot when it rebuilds after a device change, so the writer
  /// thread keeps reading the one it was given at start.
  init(
    sink: LaneFrameSink, relay: FrameRelay, configuration: Configuration,
    levels: LevelSlot? = nil
  ) {
    self.sink = sink
    self.relay = relay
    self.configuration = configuration
    let frameSize = configuration.frameSize
    let micIndex = configuration.lanes.firstIndex(of: .mic)
    let systemIndex = configuration.lanes.firstIndex(of: .system)
    self.micIndex = micIndex
    self.systemIndex = systemIndex
    laneBuffers = configuration.lanes.map { _ in
      let pointer = UnsafeMutablePointer<Float>.allocate(capacity: frameSize)
      pointer.initialize(repeating: 0, count: frameSize)
      return pointer
    }
    conversion =
      configuration.deviceRate == StenoAudio.sampleRate
      ? nil
      : Conversion(
        laneCount: configuration.lanes.count, frameSize: frameSize,
        deviceRate: configuration.deviceRate)
    processedMic = .allocate(capacity: frameSize)
    processedMic.initialize(repeating: 0, count: frameSize)
    delayedFar = .allocate(capacity: frameSize)
    delayedFar.initialize(repeating: 0, count: frameSize)
    var outputs = laneBuffers
    if let canceller = configuration.echoCanceller, let micIndex, let systemIndex {
      aec = (canceller, micIndex, systemIndex)
      outputs[micIndex] = processedMic
    } else {
      aec = nil
    }
    self.outputs = outputs
    if aec != nil, configuration.farEndDelayFrames > 0 {
      let line = LaneRingBuffer(capacity: configuration.farEndDelayFrames + frameSize)
      line.writeZeros(count: configuration.farEndDelayFrames)
      delayLine = line
    } else {
      delayLine = nil
    }
    meters = configuration.lanes.map { _ in LevelMeter() }
    self.levels = levels ?? LevelSlot(hasSystem: systemIndex != nil)
  }

  deinit {
    for buffer in laneBuffers { buffer.deallocate() }
    processedMic.deallocate()
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

  /// Processes every whole frame the rings hold right now; with a
  /// conversion, everything they hold, and the converted remainder of less
  /// than a frame waits for the next drain. Internal (not private) only so
  /// `RealTimeAllocationTests` can run the loop body on the test's own
  /// thread under an allocation hook; production calls it from `run()`
  /// alone.
  @inline(__always)
  func drain() {
    if let conversion {
      drainConverted(conversion)
      return
    }
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

  /// Reads every lane in equal counts, converts them in step and processes
  /// each whole 48 kHz frame.
  @inline(__always)
  private func drainConverted(_ conversion: Conversion) {
    let frameSize = configuration.frameSize
    while true {
      let count = min(sink.availableToRead, conversion.read)
      if count == 0 { return }
      var written = 0
      var index = 0
      while index < laneBuffers.count {
        sink.ring(index).read(into: conversion.input, count: count)
        written = conversion.converters[index].process(
          conversion.input, count: count,
          into: conversion.output[index] + conversion.pending)
        index += 1
      }
      conversion.pending += written
      var start = 0
      while conversion.pending - start >= frameSize {
        index = 0
        while index < laneBuffers.count {
          laneBuffers[index].update(from: conversion.output[index] + start, count: frameSize)
          index += 1
        }
        start += frameSize
        processFrame()
      }
      index = 0
      while index < laneBuffers.count {
        conversion.output[index].update(
          from: conversion.output[index] + start, count: conversion.pending - start)
        index += 1
      }
      conversion.pending -= start
    }
  }

  @inline(__always)
  private func processFrame() {
    let frameSize = configuration.frameSize
    if let aec {
      let system = laneBuffers[aec.system]
      var farEnd = UnsafeBufferPointer<Float>(start: system, count: frameSize)
      if let delayLine {
        delayLine.write(system, count: frameSize)
        delayLine.read(into: delayedFar, count: frameSize)
        farEnd = UnsafeBufferPointer(start: delayedFar, count: frameSize)
      }
      aec.canceller.process(
        nearEnd: UnsafeBufferPointer(start: laneBuffers[aec.mic], count: frameSize),
        farEnd: farEnd,
        out: UnsafeMutableBufferPointer(start: processedMic, count: frameSize))
    }

    // Metering on what is written.
    var index = 0
    while index < outputs.count {
      meters[index].accumulate(outputs[index], count: frameSize)
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
    while index < outputs.count {
      relay.write(channel: index, from: outputs[index])
      index += 1
    }
    if configuration.keepRawMic, let micIndex {
      relay.write(channel: outputs.count, from: laneBuffers[micIndex])
    }
    relay.endFrame()
  }
}

/// The rate conversion in front of the frames, one converter per lane.
private final class Conversion: @unchecked Sendable {
  let converters: [RateConverter]
  /// One read of device samples: a frame's worth.
  let read: Int
  /// One read's samples, reused lane after lane.
  let input: UnsafeMutablePointer<Float>
  /// Converted samples per lane; the first `pending` are not yet framed.
  let output: [UnsafeMutablePointer<Float>]
  var pending = 0

  init(laneCount: Int, frameSize: Int, deviceRate: Double) {
    let read = Int((Double(frameSize) * deviceRate / StenoAudio.sampleRate).rounded(.up))
    self.read = read
    converters = (0..<laneCount).map { _ in
      RateConverter(inputRate: deviceRate, outputRate: StenoAudio.sampleRate, maximumInput: read)
    }
    let outputCapacity = frameSize + converters[0].maximumOutput
    input = .allocate(capacity: read)
    input.initialize(repeating: 0, count: read)
    output = (0..<laneCount).map { _ in
      let pointer = UnsafeMutablePointer<Float>.allocate(capacity: outputCapacity)
      pointer.initialize(repeating: 0, count: outputCapacity)
      return pointer
    }
  }

  deinit {
    input.deallocate()
    for buffer in output { buffer.deallocate() }
  }
}
