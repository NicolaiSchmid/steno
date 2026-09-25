import Foundation
import StenoCore

/// The state machine over a `CaptureBackend`: `idle → starting → recording →
/// stopping → idle`, or `failed` when a device disappears or the writer
/// fails. Owns the sink, the processing thread, the relay, the writer thread
/// and the `RecordingWriter`; `stop()` tears them down in order (backend,
/// processing, writer, files) and returns the finished `AudioAsset`
/// (`.caf48kFloat32`, `sidecars16k` filled, retention `.keepForever` until
/// the caller sets it from `Settings`) with statistics.
///
/// After `.failed(.deviceLost)` the partial recording is finalised and
/// `stop()` returns it instead of throwing, so the app can enqueue what was
/// captured.
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
    var layout: RecordingLayout
    var sink: LaneFrameSink
    var relay: FrameRelay
    var processing: ProcessingThread
    var writerThread: WriterThread
    var writer: RecordingWriter
    var deviceChanges = 0
  }

  private var active: Active?
  private var lastResult: (asset: AudioAsset, statistics: CaptureStatistics)?

  /// `echoCanceller` nil in `.call` with `echoCancellation` on means the
  /// default `SpeexEchoCanceller`; `.inPerson` never cancels.
  /// `writerHeadroomFrames` is the relay depth between processing and file
  /// I/O; a test that feeds audio faster than real time raises it so a slow
  /// disk in a debug build is not mistaken for a drop.
  public init(
    configuration: CaptureConfiguration,
    backend: any CaptureBackend = LiveCaptureBackend(),
    echoCanceller: (any EchoCanceller)? = nil,
    writerHeadroomFrames: Int = 200
  ) throws {
    self.configuration = configuration
    self.backend = backend
    self.writerHeadroomFrames = writerHeadroomFrames
    if configuration.usesEchoCancellation {
      self.echoCanceller =
        try echoCanceller
        ?? CaptureSession.defaultEchoCanceller(
          sampleRate: StenoAudio.sampleRate, frameSize: StenoAudio.frameSize)
    } else {
      self.echoCanceller = nil
    }
  }

  /// SpeexDSP with the 200 ms tail.
  static func defaultEchoCanceller(sampleRate: Double, frameSize: Int) throws -> any EchoCanceller {
    try SpeexEchoCanceller(sampleRate: sampleRate, frameSize: frameSize)
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

  private func removeStateContinuation(_ id: UUID) { stateContinuations[id] = nil }
  private func removeLevelContinuation(_ id: UUID) { levelContinuations[id] = nil }

  private func publish(_ levels: LaneLevels) {
    latestLevels = levels
    for continuation in levelContinuations.values { continuation.yield(levels) }
  }

  /// The far-end delay the live backend's latency asks for: only above 100 ms.
  private func farEndDelayFrames() -> Int {
    guard configuration.usesEchoCancellation else { return 0 }
    var latency = 0
    if let live = backend as? LiveCaptureBackend { latency = live.inputLatencyFrames }
    return latency > Int(0.1 * StenoAudio.sampleRate) ? latency : 0
  }

  public func start(meetingID: UUID) async throws {
    switch state {
    case .idle, .failed: break
    default: throw CaptureError.invalidState("start while \(state)")
    }
    state = .starting
    let lanes = configuration.mode.lanes
    let layout = RecordingLayout(audioFolder: configuration.outputDirectory, meetingID: meetingID)
    let keepRaw = configuration.keepRawMicLane && lanes.contains(.mic)

    let writer: RecordingWriter
    do {
      writer = try RecordingWriter(layout: layout, lanes: lanes, keepRawMic: keepRaw)
    } catch {
      let failure = CaptureError.writerFailed(String(describing: error))
      state = .failed(failure)
      throw failure
    }

    let sink = LaneFrameSink(lanes: lanes) { [weak self] in
      Task { await self?.deviceLost() }
    }
    do {
      try backend.start(lanes: lanes, inputDeviceUID: configuration.inputDeviceUID, sink: sink)
    } catch {
      _ = try? writer.finish()
      try? FileManager.default.removeItem(at: layout.directory)
      let failure = (error as? CaptureError) ?? .backendFailed(String(describing: error))
      state = .failed(failure)
      throw failure
    }

    let relay = FrameRelay(
      channels: lanes.count + (keepRaw ? 1 : 0), frameSize: StenoAudio.frameSize,
      capacityFrames: writerHeadroomFrames)
    let processing = ProcessingThread(
      sink: sink, relay: relay,
      configuration: .init(
        lanes: lanes, echoCanceller: echoCanceller, farEndDelayFrames: farEndDelayFrames(),
        keepRawMic: keepRaw))
    let writerThread = WriterThread(
      relay: relay, writer: writer, levels: processing.levels, laneCount: lanes.count,
      hasRawMic: keepRaw,
      onLevels: { [weak self] levels in Task { await self?.publish(levels) } },
      onError: { [weak self] error in Task { await self?.writerFailed(error) } })
    writerThread.start()
    processing.start()

    let startedAt = Date()
    active = Active(
      meetingID: meetingID, startedAt: startedAt, layout: layout, sink: sink, relay: relay,
      processing: processing, writerThread: writerThread, writer: writer)
    state = .recording(startedAt: startedAt)
  }

  public func stop() async throws -> (asset: AudioAsset, statistics: CaptureStatistics) {
    switch state {
    case .recording:
      break
    case .failed:
      if let lastResult { return lastResult }
      throw CaptureError.invalidState("stop after a failed start")
    default:
      throw CaptureError.invalidState("stop while \(state)")
    }
    state = .stopping
    let result = try finish()
    state = .idle
    return result
  }

  /// Backend off, rings drained, relay drained, files closed, asset built.
  private func finish() throws -> (asset: AudioAsset, statistics: CaptureStatistics) {
    guard let active else { throw CaptureError.invalidState("nothing to finish") }
    self.active = nil
    backend.stop()
    active.processing.stop()
    active.writerThread.stop()
    active.sink.clear()
    let files: RecordingFiles
    do {
      files = try active.writer.finish()
    } catch {
      throw CaptureError.writerFailed(String(describing: error))
    }
    let lanes = configuration.mode.lanes
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
      systemLaneSilent: lanes.contains(.system) && active.processing.systemPeak < 1e-4,
      deviceChanges: active.deviceChanges)
    let asset = AudioAsset(
      id: UUID(), meetingID: active.meetingID, url: files.master, format: .caf48kFloat32,
      lanes: lanes, sidecars16k: files.sidecars16k, retention: .keepForever)
    let result = (asset, statistics)
    lastResult = result
    return result
  }

  private func deviceLost() {
    guard case .recording = state, var active else { return }
    active.deviceChanges += 1
    self.active = active
    state = .stopping
    do {
      _ = try finish()
      if var result = lastResult {
        result.statistics.deviceChanges = active.deviceChanges
        lastResult = result
      }
      state = .failed(.deviceLost)
    } catch {
      state = .failed((error as? CaptureError) ?? .deviceLost)
    }
  }

  private func writerFailed(_ error: any Error) {
    guard case .recording = state else { return }
    state = .stopping
    _ = try? finish()
    state = .failed(.writerFailed(String(describing: error)))
  }
}
