import Foundation
import StenoCore
import Synchronization
import Testing

@testable import StenoAudio

/// The session over the synthetic backend: state machine, level stream,
/// files, drop accounting, device loss. Everything runs as fast as the rings
/// accept; no wall-clock sleeps.
@Suite(.timeLimit(.minutes(2))) struct CaptureSessionTests {
  func configuration(_ mode: CaptureMode, in directory: URL, keepRaw: Bool = false)
    -> CaptureConfiguration
  {
    CaptureConfiguration(
      mode: mode, echoCancellation: true, keepRawMicLane: keepRaw, outputDirectory: directory)
  }

  /// Collects states until the predicate matches or the stream ends.
  func collectStates(
    _ stream: AsyncStream<CaptureState>, until done: @escaping (CaptureState) -> Bool
  )
    async -> [CaptureState]
  {
    var seen: [CaptureState] = []
    for await state in stream {
      seen.append(state)
      if done(state) { break }
    }
    return seen
  }

  @Test func idleStartingRecordingStoppingIdleOverTheSyntheticBackend() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let backend = SyntheticCaptureBackend(
      lanes: [.mic, .system], tone: [.mic: 440, .system: 1_000], seconds: 3)
    // Ten seconds of writer headroom: the backend delivers three seconds of
    // audio in milliseconds, far faster than a debug-build writer.
    let session = try CaptureSession(
      configuration: configuration(.call, in: directory), backend: backend,
      echoCanceller: try PassthroughEchoCanceller(sampleRate: 48_000, frameSize: 480),
      writerHeadroomFrames: 1_000)
    let states = await session.states
    let levels = await session.levels
    let meetingID = UUID()
    #expect(await session.state == .idle)

    try await session.start(meetingID: meetingID)
    guard case .recording = await session.state else {
      Issue.record("expected .recording, got \(await session.state)")
      return
    }

    // The level stream carries the injected tone level (amplitude 0.5 sines
    // are -9.03 dBFS). The synthetic backend runs far ahead of wall time, so
    // the writer publishes fewer than ten updates a second here.
    var iterator = levels.makeAsyncIterator()
    let first = try #require(await iterator.next())
    #expect(abs(first.mic.rms - -9.03) < 0.2)
    #expect(abs((first.system?.rms ?? 0) - -9.03) < 0.2)

    // The backend finishing its three seconds makes the duration exact; no
    // wall-clock sleep.
    await backend.waitUntilFinished()
    let result = try await session.stop()
    #expect(await session.state == .idle)

    var idles = 0
    let seen = await collectStates(states) { state in
      if state == .idle { idles += 1 }
      return idles == 2
    }
    #expect(seen.first == .idle)
    let transitions = seen.map { state -> String in
      switch state {
      case .idle: "idle"
      case .starting: "starting"
      case .recording: "recording"
      case .stopping: "stopping"
      case .failed: "failed"
      }
    }
    #expect(transitions == ["idle", "starting", "recording", "stopping", "idle"])

    #expect(result.asset.meetingID == meetingID)
    #expect(result.asset.format == .caf48kFloat32)
    #expect(result.asset.lanes == [.mic, .system])
    #expect(result.asset.retention == .keepForever)
    let layout = RecordingLayout(audioFolder: directory, meetingID: meetingID)
    #expect(result.asset.url == layout.master(.caf48kFloat32))
    #expect(
      result.asset.sidecars16k == [.mic: layout.sidecar(.mic), .system: layout.sidecar(.system)])
    #expect(RecordingLayout(asset: result.asset) == layout)
    #expect(result.statistics.droppedFrames == [:])
    #expect(abs(result.statistics.duration - 3) < 0.02)
    #expect(!result.statistics.systemLaneSilent)
    #expect(!result.statistics.endedOnDeviceLoss)

    let master = try CAFFile.read(result.asset.url)
    #expect(master.channels.count == 2)
    #expect(abs(master.duration - 3) < 0.02)
    let mic = try WAVAudioDecoder.read(result.asset.sidecars16k[.mic]!)
    #expect(abs(mic.duration - 3) < 0.02)
  }

  @Test func deviceLostStopsCleanlyWithAReadableMaster() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let backend = SyntheticCaptureBackend(
      lanes: [.mic, .system], tone: [.mic: 440, .system: 1_000], seconds: 10, loseDeviceAfter: 1)
    let session = try CaptureSession(
      configuration: configuration(.call, in: directory), backend: backend,
      echoCanceller: try PassthroughEchoCanceller(sampleRate: 48_000, frameSize: 480))
    let states = await session.states
    let meetingID = UUID()
    try await session.start(meetingID: meetingID)
    let seen = await collectStates(states) { state in
      if case .failed = state { return true }
      return false
    }
    #expect(seen.last?.failure == .deviceLost)
    #expect(seen.contains(.stopping))

    // The state carries the finalised partial recording; `stop()` returns
    // the same one.
    let result = try await session.stop()
    #expect(seen.last == .failed(.deviceLost, recording: result))
    #expect(result.statistics.endedOnDeviceLoss)
    #expect(abs(result.statistics.duration - 1) < 0.05)
    let master = try CAFFile.read(result.asset.url)
    #expect(master.channels.count == 2)
    #expect(abs(master.duration - 1) < 0.05)
    #expect(await session.state == .failed(.deviceLost, recording: result))

    // A failed session restarts (and this backend loses its device again).
    try await session.start(meetingID: UUID())
    _ = try await session.stop()
    let restarted = await session.state
    #expect(restarted == .idle || restarted.failure == .deviceLost)
  }

  /// A tap that never delivers anything (permission denied, a muted mix) is
  /// reported through `systemLaneSilent` and the level stream's floor, while
  /// the recording itself completes.
  @Test func aSilentSystemLaneIsReportedInStatisticsAndLevels() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let backend = SyntheticCaptureBackend(
      signals: [.mic: SyntheticLane(frequency: 440), .system: .silence], seconds: 1)
    let session = try CaptureSession(
      configuration: configuration(.call, in: directory), backend: backend,
      echoCanceller: try PassthroughEchoCanceller(sampleRate: 48_000, frameSize: 480),
      writerHeadroomFrames: 1_000)
    let levels = await session.levels
    try await session.start(meetingID: UUID())
    var iterator = levels.makeAsyncIterator()
    let first = try #require(await iterator.next())
    #expect(abs(first.mic.rms - -9.03) < 0.2)
    #expect(first.system == LaneLevel.silence)
    await backend.waitUntilFinished()
    let result = try await session.stop()
    #expect(result.statistics.systemLaneSilent)
    #expect(result.statistics.droppedFrames == [:])
    let master = try CAFFile.read(result.asset.url)
    #expect(master.channels[1].allSatisfy { $0 == 0 })
    #expect(master.channels[0].contains { $0 != 0 })
  }

  /// One frame of relay headroom against a backend that delivers two seconds
  /// in milliseconds: the writer falls behind, and every frame it missed is
  /// counted against the master that was written, on every lane alike.
  @Test func droppedFramesAccountForEveryFrameTheWriterMissed() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let backend = SyntheticCaptureBackend(
      lanes: [.mic, .system], tone: [.mic: 440, .system: 1_000], seconds: 2)
    let session = try CaptureSession(
      configuration: configuration(.call, in: directory), backend: backend,
      echoCanceller: try PassthroughEchoCanceller(sampleRate: 48_000, frameSize: 480),
      writerHeadroomFrames: 1)
    try await session.start(meetingID: UUID())
    await backend.waitUntilFinished()
    let result = try await session.stop()
    let master = try CAFFile.read(result.asset.url)
    let processedFrames = 200
    for lane in [AudioLane.mic, .system] {
      let dropped = result.statistics.droppedFrames[lane] ?? 0
      #expect(
        dropped + master.frameCount / 480 == processedFrames,
        "\(lane.rawValue): \(dropped) dropped + \(master.frameCount / 480) written")
    }
    #expect(result.statistics.duration == Double(master.frameCount) / 48_000)
    let sidecar = try WAVAudioDecoder.read(result.asset.sidecars16k[.mic]!)
    #expect(sidecar.samples.count == master.frameCount / 3, "sidecars drop with the master")
  }

  /// After device loss every `stop()` returns the same finished recording,
  /// and stopping the backend again is harmless.
  @Test func stopAfterDeviceLossReturnsTheSameRecordingEveryTime() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let backend = SyntheticCaptureBackend(
      lanes: [.mic, .system], tone: [.mic: 440, .system: 1_000], seconds: 10,
      loseDeviceAfter: 0.5)
    let session = try CaptureSession(
      configuration: configuration(.call, in: directory), backend: backend,
      echoCanceller: try PassthroughEchoCanceller(sampleRate: 48_000, frameSize: 480))
    let states = await session.states
    try await session.start(meetingID: UUID())
    _ = await collectStates(states) { state in
      if case .failed = state { return true }
      return false
    }
    let first = try await session.stop()
    let second = try await session.stop()
    #expect(first == second)
    #expect(await session.state == .failed(.deviceLost, recording: first))
    backend.stop()
    backend.stop()
    #expect(
      try CAFFile.read(first.asset.url).frameCount
        == Int((first.statistics.duration * 48_000).rounded()))
  }

  @Test func inPersonProducesOneChannelAndTheMixedSidecar() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let backend = SyntheticCaptureBackend(lanes: [.mixed], tone: [.mixed: 440], seconds: 1)
    let session = try CaptureSession(
      configuration: configuration(.inPerson, in: directory), backend: backend)
    let meetingID = UUID()
    try await session.start(meetingID: meetingID)
    await backend.waitUntilFinished()
    let result = try await session.stop()
    #expect(result.asset.lanes == [.mixed])
    #expect(result.asset.sidecars16k.keys.map(\.rawValue) == ["mixed"])
    #expect(try CAFFile.read(result.asset.url).channels.count == 1)
    #expect(result.statistics.systemLaneSilent == false, "no system lane, so not 'silent'")
    let levels = await session.levels
    var iterator = levels.makeAsyncIterator()
    let latest = await iterator.next()
    #expect(latest?.system == nil)
  }

  @Test func rawMicLaneIsKeptWhenAsked() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let backend = SyntheticCaptureBackend(
      lanes: [.mic, .system], tone: [.mic: 440, .system: 1_000], seconds: 0.5)
    let session = try CaptureSession(
      configuration: configuration(.call, in: directory, keepRaw: true), backend: backend,
      echoCanceller: try PassthroughEchoCanceller(sampleRate: 48_000, frameSize: 480))
    let meetingID = UUID()
    try await session.start(meetingID: meetingID)
    await backend.waitUntilFinished()
    let result = try await session.stop()
    let raw = RecordingLayout(asset: result.asset).directory.appendingPathComponent("mic.raw.caf")
    #expect(FileManager.default.fileExists(atPath: raw.path))
    #expect(try CAFFile.read(raw).channels[0] == CAFFile.read(result.asset.url).channels[0])
  }

  @Test func startWhileRecordingAndStopWhileIdleThrow() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let backend = SyntheticCaptureBackend(lanes: [.mixed], tone: [.mixed: 440], seconds: 1)
    let session = try CaptureSession(
      configuration: configuration(.inPerson, in: directory), backend: backend)
    await #expect(throws: CaptureError.self) { try await session.stop() }
    try await session.start(meetingID: UUID())
    await #expect(throws: CaptureError.self) { try await session.start(meetingID: UUID()) }
    _ = try await session.stop()
  }

  /// Fifty start/stop cycles on one session: every cycle ends idle with a
  /// readable master and nothing carries over (rings cleared, threads
  /// joined). The 200-cycle leaks and AudioObjectID check on real devices is
  /// the plan's manual step.
  @Test func repeatedStartStopCyclesStayClean() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let backend = SyntheticCaptureBackend(
      lanes: [.mic, .system], tone: [.mic: 440, .system: 1_000], seconds: 0.1)
    let session = try CaptureSession(
      configuration: configuration(.call, in: directory), backend: backend)
    for cycle in 0..<50 {
      let meetingID = UUID()
      try await session.start(meetingID: meetingID)
      await backend.waitUntilFinished()
      let result = try await session.stop()
      #expect(await session.state == .idle)
      #expect(abs(result.statistics.duration - 0.1) < 0.02, "cycle \(cycle)")
      #expect(result.statistics.droppedFrames == [:], "cycle \(cycle)")
      #expect(try CAFFile.read(result.asset.url).channels.count == 2)
    }
    #expect(try FileManager.default.contentsOfDirectory(atPath: directory.path).count == 50)
  }

  /// A backend that records once and fails on every later start.
  final class OnceThenFailing: CaptureBackend, @unchecked Sendable {
    let inner: SyntheticCaptureBackend
    private let starts = Atomic<Int>(0)

    init(_ inner: SyntheticCaptureBackend) { self.inner = inner }

    func start(lanes: [AudioLane], inputDeviceUID: String?, sink: LaneFrameSink) throws
      -> CaptureStream
    {
      guard starts.wrappingAdd(1, ordering: .relaxed).oldValue == 0 else {
        throw CaptureError.inputDeviceUnavailable
      }
      return try inner.start(lanes: lanes, inputDeviceUID: inputDeviceUID, sink: sink)
    }

    func stop() { inner.stop() }
  }

  /// Meeting A records and stops; meeting B's start fails in the backend.
  /// `stop()` after that failure must throw, not hand out A's asset under
  /// B's meeting (the app would enqueue A twice).
  @Test func aFailedRestartDoesNotReturnThePreviousMeetingsRecording() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let backend = OnceThenFailing(
      SyntheticCaptureBackend(lanes: [.mixed], tone: [.mixed: 440], seconds: 0.2))
    let session = try CaptureSession(
      configuration: configuration(.inPerson, in: directory), backend: backend)
    let first = UUID()
    try await session.start(meetingID: first)
    await backend.inner.waitUntilFinished()
    let result = try await session.stop()
    #expect(result.asset.meetingID == first)

    await #expect(throws: CaptureError.inputDeviceUnavailable) {
      try await session.start(meetingID: UUID())
    }
    #expect(await session.state == .failed(.inputDeviceUnavailable, recording: nil))
    await #expect(throws: CaptureError.self, "nothing was recorded for this start") {
      _ = try await session.stop()
    }
  }

  /// Logs `reset` and `process` calls in order.
  final class ResetLoggingCanceller: EchoCanceller, @unchecked Sendable {
    let sampleRate: Double
    let frameSize: Int
    private let lock = NSLock()
    private var entries: [String] = []

    init(sampleRate: Double, frameSize: Int) throws {
      self.sampleRate = sampleRate
      self.frameSize = frameSize
    }

    var log: [String] {
      lock.lock()
      defer { lock.unlock() }
      return entries
    }

    func process(
      nearEnd: UnsafeBufferPointer<Float>, farEnd: UnsafeBufferPointer<Float>,
      out: UnsafeMutableBufferPointer<Float>
    ) {
      lock.lock()
      if entries.last != "process" { entries.append("process") }
      lock.unlock()
      for index in 0..<min(nearEnd.count, out.count) { out[index] = nearEnd[index] }
    }

    func reset() {
      lock.lock()
      entries.append("reset")
      lock.unlock()
    }
  }

  /// One canceller serves every recording of a session, so each start resets
  /// it before the first frame: meeting two on headphones must not begin
  /// with the filter meeting one converged on the loudspeakers.
  @Test func everyStartResetsTheEchoCancellerBeforeTheFirstFrame() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let backend = SyntheticCaptureBackend(
      lanes: [.mic, .system], tone: [.mic: 440, .system: 1_000], seconds: 0.1)
    let canceller = try ResetLoggingCanceller(sampleRate: 48_000, frameSize: 480)
    let session = try CaptureSession(
      configuration: configuration(.call, in: directory), backend: backend,
      echoCanceller: canceller)
    for _ in 0..<2 {
      try await session.start(meetingID: UUID())
      await backend.waitUntilFinished()
      _ = try await session.stop()
    }
    #expect(canceller.log == ["reset", "process", "reset", "process"])
  }

  /// The far-end is delayed by both device paths (mic input, speaker output)
  /// whenever their sum reaches one processing frame; the Speex tail keeps
  /// the room. A 30 ms input path alone used to stay under the old 100 ms
  /// threshold while a Bluetooth output pushed the echo past the tail.
  @Test func farEndDelayCoversInputAndOutputPathsFromOneFrameUp() {
    #expect(CaptureSession.farEndDelayFrames(inputLatencyFrames: 0, outputLatencyFrames: 0) == 0)
    #expect(
      CaptureSession.farEndDelayFrames(inputLatencyFrames: 200, outputLatencyFrames: 200) == 0,
      "under one frame the tail absorbs it")
    #expect(
      CaptureSession.farEndDelayFrames(inputLatencyFrames: 300, outputLatencyFrames: 300) == 600)
    #expect(
      CaptureSession.farEndDelayFrames(inputLatencyFrames: 1_440, outputLatencyFrames: 9_600)
        == 11_040, "a 30 ms mic path plus a 200 ms Bluetooth output")
    #expect(
      CaptureSession.farEndDelayFrames(inputLatencyFrames: 0, outputLatencyFrames: 480) == 480)
  }

  /// The backend describes the stream it opened; the session keeps it while
  /// recording (capture-spike prints it) and drops it with the recording.
  @Test func theSessionExposesTheBackendsStreamWhileRecording() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let backend = SyntheticCaptureBackend(lanes: [.mixed], tone: [.mixed: 440], seconds: 0.1)
    let session = try CaptureSession(
      configuration: configuration(.inPerson, in: directory), backend: backend)
    #expect(await session.stream == nil)
    try await session.start(meetingID: UUID())
    #expect(await session.stream == .synthetic)
    #expect(await session.stream?.sampleRate == StenoAudio.sampleRate)
    await backend.waitUntilFinished()
    _ = try await session.stop()
    #expect(await session.stream == nil)
  }

  /// Wraps the real writer and fails where a full disk would: every `write`
  /// after `failAfterFrames`, and `finish()` itself when asked.
  final class FaultyWriter: RecordingWriting, @unchecked Sendable {
    struct DiskFull: Error {}
    let inner: RecordingWriter
    let failAfterFrames: Int?
    let failFinish: Bool
    private var frames = 0

    init(_ inner: RecordingWriter, failAfterFrames: Int? = nil, failFinish: Bool) {
      self.inner = inner
      self.failAfterFrames = failAfterFrames
      self.failFinish = failFinish
    }

    var files: RecordingFiles { inner.files }

    func write(_ frames: LaneFrames) throws {
      self.frames += 1
      if let failAfterFrames, self.frames > failAfterFrames { throw DiskFull() }
      try inner.write(frames)
    }

    func finish() throws -> RecordingFiles {
      let files = try inner.finish()
      if failFinish { throw DiskFull() }
      return files
    }
  }

  func faultySession(
    in directory: URL, backend: SyntheticCaptureBackend, failAfterFrames: Int? = nil,
    failFinish: Bool
  ) throws -> CaptureSession {
    try CaptureSession(
      configuration: configuration(.inPerson, in: directory), backend: backend,
      echoCanceller: nil, writerHeadroomFrames: 1_000,
      makeWriter: { layout, lanes, keepRaw in
        FaultyWriter(
          try RecordingWriter(layout: layout, lanes: lanes, keepRawMic: keepRaw),
          failAfterFrames: failAfterFrames, failFinish: failFinish)
      })
  }

  /// The disk fills while closing the files: `stop()` still returns the
  /// asset (the master is readable to its last frame) and the state, not a
  /// thrown error, carries the failure. It used to throw and strand the
  /// session in `.stopping`.
  @Test func aFailingFinishStillReturnsTheAssetAndEndsFailed() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let backend = SyntheticCaptureBackend(lanes: [.mixed], tone: [.mixed: 440], seconds: 0.5)
    let session = try faultySession(in: directory, backend: backend, failFinish: true)
    let meetingID = UUID()
    try await session.start(meetingID: meetingID)
    await backend.waitUntilFinished()
    let result = try await session.stop()
    #expect(result.asset.meetingID == meetingID)
    #expect(abs(result.statistics.duration - 0.5) < 0.02)
    #expect(try CAFFile.read(result.asset.url).frameCount == 24_000)
    guard case .failed(.writerFailed(let detail), let recording) = await session.state else {
      Issue.record("expected .failed(.writerFailed), got \(await session.state)")
      return
    }
    #expect(detail.contains("DiskFull"))
    #expect(recording == result, "the failed state carries the recording")
    #expect(try await session.stop() == result, "and stop() returns the same one")
  }

  /// The disk fills mid-recording: the first write error stops the writes,
  /// the session finalises, and `stop()` returns what was written.
  @Test func aFailingWriteMidRecordingFinalisesWhatWasWritten() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let backend = SyntheticCaptureBackend(lanes: [.mixed], tone: [.mixed: 440], seconds: 2)
    let session = try faultySession(
      in: directory, backend: backend, failAfterFrames: 30, failFinish: false)
    let states = await session.states
    try await session.start(meetingID: UUID())
    let seen = await collectStates(states) { state in
      if case .failed = state { return true }
      return false
    }
    guard case .failed(.writerFailed, _) = seen.last else {
      Issue.record("expected .failed(.writerFailed), got \(String(describing: seen.last))")
      return
    }
    let result = try await session.stop()
    #expect(abs(result.statistics.duration - 0.3) < 0.001)
    #expect(try CAFFile.read(result.asset.url).frameCount == 30 * 480)
  }

  @Test func aFailingBackendLeavesTheSessionFailedAndNoFolder() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    struct Failing: CaptureBackend {
      func start(lanes: [AudioLane], inputDeviceUID: String?, sink: LaneFrameSink) throws
        -> CaptureStream
      {
        throw CaptureError.inputDeviceUnavailable
      }
      func stop() {}
    }
    let session = try CaptureSession(
      configuration: configuration(.inPerson, in: directory), backend: Failing())
    let meetingID = UUID()
    await #expect(throws: CaptureError.inputDeviceUnavailable) {
      try await session.start(meetingID: meetingID)
    }
    #expect(await session.state == .failed(.inputDeviceUnavailable, recording: nil))
    let layout = RecordingLayout(audioFolder: directory, meetingID: meetingID)
    #expect(!FileManager.default.fileExists(atPath: layout.directory.path))
    await #expect(throws: CaptureError.self) { try await session.stop() }
  }
}
