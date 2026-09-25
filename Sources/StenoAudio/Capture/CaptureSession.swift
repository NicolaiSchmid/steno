import Foundation
import StenoCore

/// The state machine over a `CaptureBackend`: `idle → starting → recording →
/// stopping → idle`, or `failed` when a device disappears or the writer
/// fails. Owns the sink, the processing thread, the relay, the writer thread
/// and the `RecordingWriter`; `stop()` tears them down in order (backend,
/// processing, writer, files) and returns the `CaptureResult`: the finished
/// `AudioAsset` (`.caf48kFloat32`, `sidecars16k` filled, retention
/// `.keepForever` until the caller sets it from `Settings`) with statistics.
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
  private let backend: any CaptureBackend
  private let echoCanceller: (any EchoCanceller)?

  public private(set) var state: CaptureState = .idle {
    didSet {
      for continuation in stateContinuations.values { continuation.yield(state) }
    }
  }

  private var stateContinuations: [UUID: AsyncStream<CaptureState>.Continuation] = [:]
  private var levelContinuations: [UUID: AsyncStream<LaneLevels>.Continuation] = [:]
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
  }

  /// Opens the files for one recording; `RecordingWriter.init` in
  /// production, a failure-injecting wrapper in tests.
  typealias WriterFactory = @Sendable (RecordingLayout, [AudioLane], _ keepRawMic: Bool) throws ->
    any RecordingWriting

  private let makeWriter: WriterFactory
  private var active: Active?

  /// `echoCanceller` nil in `.call` with `echoCancellation` on means
  /// `SpeexEchoCanceller` with the 200 ms tail; `.inPerson` never cancels.
  /// `writerHeadroomFrames` is the relay depth between processing and file
  /// I/O; a test that feeds audio faster than real time raises it so a slow
  /// disk in a debug build is not mistaken for a drop.
  public init(
    configuration: CaptureConfiguration,
    backend: any CaptureBackend = LiveCaptureBackend(),
    echoCanceller: (any EchoCanceller)? = nil,
    writerHeadroomFrames: Int = 200
  ) throws {
    try self.init(
      configuration: configuration, backend: backend, echoCanceller: echoCanceller,
      writerHeadroomFrames: writerHeadroomFrames,
      makeWriter: { layout, lanes, keepRawMic in
        try RecordingWriter(layout: layout, lanes: lanes, keepRawMic: keepRawMic)
      })
  }

  init(
    configuration: CaptureConfiguration,
    backend: any CaptureBackend,
    echoCanceller: (any EchoCanceller)?,
    writerHeadroomFrames: Int,
    makeWriter: @escaping WriterFactory
  ) throws {
    self.configuration = configuration
    self.backend = backend
    self.writerHeadroomFrames = writerHeadroomFrames
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

  /// The stream the backend opened for the current recording; nil while not
  /// recording. `steno dev capture-spike` prints it.
  public var stream: CaptureStream? { active?.stream }

  private func removeStateContinuation(_ id: UUID) { stateContinuations[id] = nil }
  private func removeLevelContinuation(_ id: UUID) { levelContinuations[id] = nil }

  private func publish(_ levels: LaneLevels) {
    latestLevels = levels
    for continuation in levelContinuations.values { continuation.yield(levels) }
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
    let keepRaw = configuration.keepRawMicLane && lanes.contains(.mic)

    let writer: any RecordingWriting
    do {
      writer = try makeWriter(layout, lanes, keepRaw)
    } catch {
      let failure = CaptureError.writerFailed(String(describing: error))
      state = .failed(failure, recording: nil)
      throw failure
    }

    let sink = LaneFrameSink(lanes: lanes) { [weak self] in
      Task { await self?.deviceLost() }
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
    let processing = ProcessingThread(
      sink: sink, relay: relay,
      configuration: .init(
        lanes: lanes, echoCanceller: echoCanceller,
        farEndDelayFrames: farEndDelayFrames(for: stream), keepRawMic: keepRaw))
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

  /// Ends the recording and returns it. After `.failed` returns the
  /// finalised partial recording the state carries, or throws when the
  /// failure came from a start that produced nothing.
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

  /// Backend off, rings drained, relay drained, files closed, asset built.
  /// The asset is built even when closing the files fails (its URLs are
  /// fixed at start and the duration is what the master holds); the failure
  /// comes back beside it.
  private func finish() -> (result: CaptureResult, failure: CaptureError?)? {
    guard let active else { return nil }
    self.active = nil
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
    let statistics = CaptureStatistics(
      duration: files.duration,
      droppedFrames: dropped,
      systemLaneSilent: lanes.contains(.system)
        && active.processing.systemPeak < LaneLevel.silentPeakLinear,
      endedOnDeviceLoss: active.endedOnDeviceLoss)
    let asset = AudioAsset(
      id: UUID(), meetingID: active.meetingID, url: files.master, format: .caf48kFloat32,
      lanes: lanes, sidecars16k: files.sidecars16k, retention: .keepForever)
    return (CaptureResult(asset: asset, statistics: statistics), failure)
  }

  private func deviceLost() {
    guard case .recording = state, var active else { return }
    active.endedOnDeviceLoss = true
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
