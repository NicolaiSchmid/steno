import Foundation
import StenoCore

/// The state machine over a `CaptureBackend`: `idle → starting → recording →
/// stopping → idle`, or `failed` when a device stays lost or the writer
/// fails. Owns the sink, the processing thread, the relay, the writer thread
/// and the `RecordingWriter`; `stop()` tears them down in order (backend,
/// processing, writer, files) and returns the `CaptureResult`: the finished
/// `AudioAsset` (`.caf48kFloat32`, `sidecars16k` filled, retention
/// `.keepForever` until the caller sets it from `Settings`) with statistics.
///
/// A device change while recording does not end the recording. The backend
/// reports it through the sink; the session stops the backend and the
/// processing thread, starts the backend again on the devices as they are
/// now (up to `restartAttempts` times, `restartBackoff` apart on `clock`),
/// fills the gap with silence through the relay so the master stays on wall
/// time, starts a processing thread built for the new latencies, and keeps
/// the sink, the relay, the writer thread, the writer and the files. The
/// state stays `.recording`; `notices` carries `.deviceChanged` and
/// `.deviceResumed`. Only when every restart fails does the recording end in
/// `.failed(.deviceLost, recording:)`.
///
/// A recording cut short (device loss, writer failure, a full disk while
/// closing) is finalised and travels in the state:
/// `.failed(error, recording: result)`; `stop()` returns the same result, or
/// throws when the failure came from a start that produced nothing.
public actor CaptureSession {
  public let configuration: CaptureConfiguration
  /// Frames the writer may fall behind the processing thread before frames
  /// are dropped and counted: 200 (2 s) by default.
  public let writerHeadroomFrames: Int
  /// Restarts tried after a device change before the recording ends in
  /// `.deviceLost`, and the waits before the second, third and fourth: a
  /// Bluetooth device is gone for one to two seconds while it changes
  /// profile; a device replugged by hand takes longer and is a loss the user
  /// can see and restart from.
  public static let restartAttempts = 4
  public static let restartBackoff: [Duration] = [
    .milliseconds(250), .milliseconds(500), .seconds(1),
  ]
  /// The most silence written for one gap; a longer outage leaves the master
  /// that much short of wall time rather than filling minutes of zeros.
  public static let maximumGap: Duration = .seconds(10)
  private let backend: any CaptureBackend
  private let echoCanceller: (any EchoCanceller)?
  private let clock: ErasedClock

  public private(set) var state: CaptureState = .idle {
    didSet {
      for continuation in stateContinuations.values { continuation.yield(state) }
    }
  }

  private var stateContinuations: [UUID: AsyncStream<CaptureState>.Continuation] = [:]
  private var levelContinuations: [UUID: AsyncStream<LaneLevels>.Continuation] = [:]
  private var noticeContinuations: [UUID: AsyncStream<CaptureNotice>.Continuation] = [:]
  private var latestLevels: LaneLevels?

  private struct Active {
    var meetingID: UUID
    var startedAt: Date
    var stream: CaptureStream
    var sink: LaneFrameSink
    var relay: FrameRelay
    var processing: ProcessingThread
    var writerThread: WriterThread
    var writer: any RecordingWriting
    var endedOnDeviceLoss = false
    /// Rebuilds that succeeded.
    var deviceChanges = 0
    /// Silence written across those rebuilds.
    var gapSeconds: TimeInterval = 0
    /// The loudest system-lane sample over every processing thread replaced
    /// so far; `finish()` takes the maximum with the current one.
    var systemPeakSoFar: Float = 0
    /// The rebuild in flight, so `stop()` can abandon it.
    var rebuild: Task<Void, Never>?
    /// Bumped per rebuild; a rebuild that wakes to another generation (a
    /// stop and a new start meanwhile) does nothing.
    var rebuildGeneration = 0
  }

  /// Opens the files for one recording; `RecordingWriter.init` in
  /// production, a failure-injecting wrapper in tests.
  typealias WriterFactory =
    @Sendable (RecordingLayout, [AudioLane], _ keepRawMic: Bool) throws ->
    any RecordingWriting

  private let makeWriter: WriterFactory
  private var active: Active?

  /// `echoCanceller` nil in `.call` with `echoCancellation` on means
  /// `SpeexEchoCanceller` with the 200 ms tail; `.inPerson` never cancels.
  /// `writerHeadroomFrames` is the relay depth between processing and file
  /// I/O; a test that feeds audio faster than real time raises it so a slow
  /// disk in a debug build is not mistaken for a drop. `clock` paces the
  /// restart backoff and measures the gap after a device change; tests pass
  /// a `ManualClock`.
  public init(
    configuration: CaptureConfiguration,
    backend: any CaptureBackend = LiveCaptureBackend(),
    echoCanceller: (any EchoCanceller)? = nil,
    writerHeadroomFrames: Int = 200,
    clock: any Clock<Duration> = ContinuousClock()
  ) throws {
    try self.init(
      configuration: configuration, backend: backend, echoCanceller: echoCanceller,
      writerHeadroomFrames: writerHeadroomFrames, clock: clock,
      makeWriter: { layout, lanes, keepRawMic in
        try RecordingWriter(layout: layout, lanes: lanes, keepRawMic: keepRawMic)
      })
  }

  init(
    configuration: CaptureConfiguration,
    backend: any CaptureBackend,
    echoCanceller: (any EchoCanceller)?,
    writerHeadroomFrames: Int,
    clock: any Clock<Duration> = ContinuousClock(),
    makeWriter: @escaping WriterFactory
  ) throws {
    self.configuration = configuration
    self.backend = backend
    self.writerHeadroomFrames = writerHeadroomFrames
    self.clock = ErasedClock(clock)
    self.makeWriter = makeWriter
    if configuration.usesEchoCancellation {
      self.echoCanceller =
        try echoCanceller
        ?? SpeexEchoCanceller(sampleRate: StenoAudio.sampleRate, frameSize: StenoAudio.frameSize)
    } else {
      self.echoCanceller = nil
    }
  }

  /// Every state change from now on, starting with the current state.
  public var states: AsyncStream<CaptureState> {
    let id = UUID()
    let current = state
    return AsyncStream { continuation in
      stateContinuations[id] = continuation
      continuation.yield(current)
      continuation.onTermination = { [weak self] _ in
        Task { await self?.removeStateContinuation(id) }
      }
    }
  }

  /// Lane levels at 10 Hz while recording.
  public var levels: AsyncStream<LaneLevels> {
    let id = UUID()
    let latest = latestLevels
    return AsyncStream { continuation in
      levelContinuations[id] = continuation
      if let latest { continuation.yield(latest) }
      continuation.onTermination = { [weak self] _ in
        Task { await self?.removeLevelContinuation(id) }
      }
    }
  }

  /// Device changes from now on, while the state stays `.recording`:
  /// `.deviceChanged(reason)` when a rebuild begins and
  /// `.deviceResumed(attempt:gapSeconds:)` when the new backend runs. Device
  /// loss is not a notice; `states` carries it.
  public var notices: AsyncStream<CaptureNotice> {
    let id = UUID()
    return AsyncStream { continuation in
      noticeContinuations[id] = continuation
      continuation.onTermination = { [weak self] _ in
        Task { await self?.removeNoticeContinuation(id) }
      }
    }
  }

  /// The stream the backend opened for the current recording, the rebuilt
  /// backend's after a device change; nil while not recording. `steno dev
  /// capture-spike` prints it.
  public var stream: CaptureStream? { active?.stream }

  private func removeStateContinuation(_ id: UUID) { stateContinuations[id] = nil }
  private func removeLevelContinuation(_ id: UUID) { levelContinuations[id] = nil }
  private func removeNoticeContinuation(_ id: UUID) { noticeContinuations[id] = nil }

  private func publish(_ levels: LaneLevels) {
    latestLevels = levels
    for continuation in levelContinuations.values { continuation.yield(levels) }
  }

  private func emit(_ notice: CaptureNotice) {
    for continuation in noticeContinuations.values { continuation.yield(notice) }
  }

  /// The far-end delay for the latencies the backend reports. The mic hears
  /// the tap's signal after the output path (output latency plus safety
  /// offset), the room and the input path (input latency plus safety
  /// offset), so the far-end is delayed by the two device paths in full and
  /// the Speex tail (200 ms) is left for the room and for what the HAL
  /// under-reports. Below one processing frame the tail absorbs the offset
  /// as well; over-delaying is the one thing the MDF filter cannot recover
  /// from, so nothing is rounded up.
  static func farEndDelayFrames(inputLatencyFrames: Int, outputLatencyFrames: Int) -> Int {
    let total = max(0, inputLatencyFrames) + max(0, outputLatencyFrames)
    return total >= StenoAudio.frameSize ? total : 0
  }

  private func farEndDelayFrames(for stream: CaptureStream) -> Int {
    guard configuration.usesEchoCancellation else { return 0 }
    return Self.farEndDelayFrames(
      inputLatencyFrames: stream.inputLatencyFrames,
      outputLatencyFrames: stream.outputLatencyFrames)
  }

  /// Whether the relay and the writer carry the raw microphone channel.
  private var keepRaw: Bool {
    configuration.keepRawMicLane && configuration.lanes.contains(.mic)
  }

  private func makeProcessingThread(
    sink: LaneFrameSink, relay: FrameRelay, stream: CaptureStream, levels: LevelSlot?
  ) -> ProcessingThread {
    ProcessingThread(
      sink: sink, relay: relay,
      configuration: .init(
        lanes: configuration.lanes, echoCanceller: echoCanceller,
        farEndDelayFrames: farEndDelayFrames(for: stream), keepRawMic: keepRaw),
      levels: levels)
  }

  public func start(meetingID: UUID) async throws {
    switch state {
    case .idle, .failed: break
    default: throw CaptureError.invalidState("start while \(state)")
    }
    // `.starting` carries no recording: a failed start must never hand out
    // the previous meeting's files.
    state = .starting
    // A new recording, possibly on other devices, starts from a cold filter.
    echoCanceller?.reset()
    let lanes = configuration.lanes
    let layout = RecordingLayout(audioFolder: configuration.outputDirectory, meetingID: meetingID)

    let writer: any RecordingWriting
    do {
      writer = try makeWriter(layout, lanes, keepRaw)
    } catch {
      let failure = CaptureError.writerFailed(String(describing: error))
      state = .failed(failure, recording: nil)
      throw failure
    }

    let sink = LaneFrameSink(lanes: lanes) { [weak self] reason in
      Task { await self?.deviceChanged(reason) }
    }
    let stream: CaptureStream
    do {
      stream = try backend.start(
        lanes: lanes, inputDeviceUID: configuration.inputDeviceUID, sink: sink)
    } catch {
      _ = try? writer.finish()
      try? FileManager.default.removeItem(at: layout.directory)
      let failure = (error as? CaptureError) ?? .backendFailed(String(describing: error))
      state = .failed(failure, recording: nil)
      throw failure
    }

    let relay = FrameRelay(
      channels: lanes.count + (keepRaw ? 1 : 0), frameSize: StenoAudio.frameSize,
      capacityFrames: writerHeadroomFrames)
    let processing = makeProcessingThread(sink: sink, relay: relay, stream: stream, levels: nil)
    let writerThread = WriterThread(
      relay: relay, writer: writer, levels: processing.levels, laneCount: lanes.count,
      hasRawMic: keepRaw,
      onLevels: { [weak self] levels in Task { await self?.publish(levels) } },
      onError: { [weak self] error in Task { await self?.writerFailed(error) } })
    writerThread.start()
    processing.start()

    let startedAt = Date()
    active = Active(
      meetingID: meetingID, startedAt: startedAt, stream: stream, sink: sink, relay: relay,
      processing: processing, writerThread: writerThread, writer: writer)
    state = .recording(startedAt: startedAt)
  }

  /// Ends the recording and returns it. A rebuild in flight is abandoned.
  /// After `.failed` returns the finalised partial recording the state
  /// carries, or throws when the failure came from a start that produced
  /// nothing.
  public func stop() async throws -> CaptureResult {
    switch state {
    case .recording:
      break
    case .failed(_, let recording):
      if let recording { return recording }
      throw CaptureError.invalidState("stop after a failed start")
    default:
      throw CaptureError.invalidState("stop while \(state)")
    }
    state = .stopping
    guard let (result, failure) = finish() else {
      throw CaptureError.invalidState("nothing to finish")
    }
    // Closing the files can fail on a full disk; the master is still
    // readable to its last frame, so the result comes back and the state
    // carries the failure instead of `stop()` throwing it away.
    state = failure.map { .failed($0, recording: result) } ?? .idle
    return result
  }

  /// Rebuild abandoned, backend off, rings drained, relay drained, files
  /// closed, asset built. The asset is built even when closing the files
  /// fails (its URLs are fixed at start and the duration is what the master
  /// holds); the failure comes back beside it.
  private func finish() -> (result: CaptureResult, failure: CaptureError?)? {
    guard let active else { return nil }
    self.active = nil
    active.rebuild?.cancel()
    backend.stop()
    active.processing.stop()
    active.writerThread.stop()
    active.sink.clear()
    var failure: CaptureError?
    do {
      try active.writer.finish()
    } catch {
      failure = CaptureError.writerFailed(String(describing: error))
    }
    let files = active.writer.files
    let lanes = configuration.lanes
    var dropped: [AudioLane: Int] = [:]
    for (lane, samples) in active.sink.droppedSamples {
      dropped[lane, default: 0] += samples / StenoAudio.frameSize
    }
    for (index, frames) in active.relay.droppedFrames.enumerated() where index < lanes.count {
      if frames > 0 { dropped[lanes[index], default: 0] += frames }
    }
    let systemPeak = max(active.systemPeakSoFar, active.processing.systemPeak)
    let statistics = CaptureStatistics(
      duration: files.duration,
      droppedFrames: dropped,
      systemLaneSilent: lanes.contains(.system) && systemPeak < LaneLevel.silentPeakLinear,
      endedOnDeviceLoss: active.endedOnDeviceLoss,
      deviceChanges: active.deviceChanges,
      gapSeconds: active.gapSeconds)
    let asset = AudioAsset(
      id: UUID(), meetingID: active.meetingID, url: files.master, format: .caf48kFloat32,
      lanes: lanes, sidecars16k: files.sidecars16k, retention: .keepForever)
    return (CaptureResult(asset: asset, statistics: statistics), failure)
  }

  // MARK: Device changes

  /// The sink's handler, on the actor. Ignored unless recording with no
  /// rebuild in flight; otherwise the notice goes out and the rebuild runs
  /// as its own task so `stop()` can interleave at its sleeps. Internal so a
  /// test can report a change while idle or stopping; production reaches it
  /// through the sink alone.
  func deviceChanged(_ reason: DeviceChangeReason) {
    guard case .recording = state, var active, active.rebuild == nil else { return }
    emit(.deviceChanged(reason))
    active.rebuildGeneration += 1
    let generation = active.rebuildGeneration
    // The task body runs on the actor once this method returns, so the
    // handle is in place before it looks for it.
    active.rebuild = Task { await self.rebuild(generation: generation) }
    self.active = active
  }

  private func stillRebuilding(_ generation: Int) -> Bool {
    guard case .recording = state, let active, active.rebuild != nil,
      active.rebuildGeneration == generation
    else { return false }
    return true
  }

  /// Old backend and processing thread off, then `start` again with backoff;
  /// the gap since the old backend stopped is written as silence before the
  /// new processing thread starts. The sink, the relay, the writer thread
  /// and the files stay. Nothing here runs on a real-time thread.
  private func rebuild(generation: Int) async {
    guard stillRebuilding(generation), let current = active else { return }
    // Whatever whole frames the rings hold are the old device's last audio;
    // the processing thread's stop drains them into the relay.
    backend.stop()
    current.processing.stop()
    var interim = current
    interim.systemPeakSoFar = max(current.systemPeakSoFar, current.processing.systemPeak)
    active = interim
    let elapsed = clock.stopwatch()
    // New devices mean a new echo path: the filter starts cold, as at start.
    echoCanceller?.reset()
    for attempt in 1...Self.restartAttempts {
      let stream: CaptureStream
      do {
        stream = try backend.start(
          lanes: configuration.lanes, inputDeviceUID: configuration.inputDeviceUID,
          sink: current.sink)
      } catch {
        guard attempt < Self.restartAttempts else { break }
        do {
          try await clock.sleep(Self.restartBackoff[attempt - 1])
        } catch {
          return  // cancelled by `stop()`
        }
        guard stillRebuilding(generation) else { return }
        continue
      }
      // The gap grows through every failed attempt and is written once, in
      // full, when a start succeeds.
      let gapFrames = Self.gapFrames(for: min(elapsed(), Self.maximumGap))
      guard await writeSilence(frames: gapFrames, into: current.relay, generation: generation),
        var updated = active
      else { return }
      let gapSeconds = Double(gapFrames * StenoAudio.frameSize) / StenoAudio.sampleRate
      let processing = makeProcessingThread(
        sink: current.sink, relay: current.relay, stream: stream, levels: current.processing.levels)
      processing.start()
      updated.stream = stream
      updated.processing = processing
      updated.deviceChanges += 1
      updated.gapSeconds += gapSeconds
      updated.rebuild = nil
      active = updated
      updated.sink.rearmDeviceChange()
      emit(.deviceResumed(attempt: attempt, gapSeconds: gapSeconds))
      return
    }
    deviceLost()
  }

  /// Whole relay frames for a gap: 48 000 samples a second in frames of
  /// `StenoAudio.frameSize`, rounded down.
  static func gapFrames(for gap: Duration) -> Int {
    let samples = (gap / .seconds(1)) * StenoAudio.sampleRate
    return max(0, Int(samples.rounded(.down)) / StenoAudio.frameSize)
  }

  /// Zeros in every written channel for `frames` relay frames, from the
  /// actor, through the relay the writer thread keeps draining. The rings
  /// under the sink are not touched: they hold two seconds and nothing
  /// drains them while the processing thread is stopped, so a longer gap
  /// would silently shrink into `droppedSamples`. A full relay (a long gap,
  /// or a writer still behind the old producer) is waited out in 5 ms steps
  /// on the clock. Returns false when the rebuild was abandoned meanwhile
  /// (a stop cancelled the wait or replaced the recording).
  private func writeSilence(frames: Int, into relay: FrameRelay, generation: Int) async -> Bool {
    guard frames > 0 else { return true }
    let zeros = [Float](repeating: 0, count: relay.frameSize)
    var remaining = frames
    while remaining > 0 {
      guard stillRebuilding(generation) else { return false }
      if relay.beginFrame() {
        zeros.withUnsafeBufferPointer { buffer in
          for channel in 0..<relay.channels {
            relay.write(channel: channel, from: buffer.baseAddress!)
          }
        }
        relay.endFrame()
        remaining -= 1
      } else {
        do {
          try await clock.sleep(.milliseconds(5))
        } catch {
          return false  // cancelled by `stop()`
        }
      }
    }
    return true
  }

  /// Every restart failed: the recording ends as it did before rebuilds
  /// existed, finalised and carried in `.failed(.deviceLost, recording:)`.
  private func deviceLost() {
    guard case .recording = state, var active else { return }
    active.endedOnDeviceLoss = true
    // This runs inside the rebuild task; `finish()` must not cancel it.
    active.rebuild = nil
    self.active = active
    state = .stopping
    guard let (result, failure) = finish() else { return }
    state = .failed(failure ?? .deviceLost, recording: result)
  }

  private func writerFailed(_ error: any Error) {
    guard case .recording = state else { return }
    state = .stopping
    let result = finish()?.result
    state = .failed(.writerFailed(String(describing: error)), recording: result)
  }
}

/// `any Clock<Duration>` cannot hand the actor an instant it can store, so
/// the clock is erased to the two things the rebuild needs: a sleep and a
/// stopwatch. Both run on the injected clock, so a test on `ManualClock`
/// knows the exact gap.
struct ErasedClock: Sendable {
  let sleep: @Sendable (Duration) async throws -> Void
  /// Starts a stopwatch; the returned closure reads the time since.
  let stopwatch: @Sendable () -> @Sendable () -> Duration

  init(_ clock: some Clock<Duration>) {
    sleep = { try await clock.sleep(for: $0) }
    stopwatch = {
      let start = clock.now
      return { start.duration(to: clock.now) }
    }
  }
}
