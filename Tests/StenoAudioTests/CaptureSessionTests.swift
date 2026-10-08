import Foundation
import StenoCore
import Synchronization
import Testing

@testable import StenoAudio

/// The session over the synthetic backend: state machine, level stream,
/// files, drop accounting, device changes and loss. Everything runs as fast
/// as the rings accept and the rebuild's backoff runs on a `ManualClock`; no
/// wall-clock sleeps.
@Suite(.timeLimit(.minutes(2))) struct CaptureSessionTests {
  func configuration(_ mode: CaptureMode, in directory: URL, keepRaw: Bool = false)
    -> CaptureConfiguration
  {
    CaptureConfiguration(
      mode: mode, echoCancellation: true, keepRawMicLane: keepRaw, outputDirectory: directory)
  }

  func kind(_ state: CaptureState) -> String {
    switch state {
    case .idle: "idle"
    case .starting: "starting"
    case .recording: "recording"
    case .stopping: "stopping"
    case .failed: "failed"
    }
  }

  /// Advances `clock` through the first `count` backoff sleeps of a rebuild
  /// (`CaptureSession.restartBackoff`), each once the sleeper is registered.
  func advance(_ clock: ManualClock, throughSleeps count: Int) async {
    for step in CaptureSession.restartBackoff.prefix(count) {
      #expect(await clock.waitForSleepers(1), "the rebuild sleeps on the injected clock")
      clock.advance(by: step)
    }
  }

  /// Advances `clock` in 5 ms steps whenever the rebuild waits out a full
  /// relay, until `done` says the rebuild is over. A fast writer may drain
  /// the relay before it ever fills, so the waits are driven as they appear
  /// rather than counted.
  func driveRelayWaits(_ clock: ManualClock, until done: () async -> Bool) async {
    while !(await done()) {
      if await clock.waitForSleepers(1, attempts: 100) {
        clock.advance(by: .milliseconds(5))
      }
    }
  }

  /// Gives every task already queued on the cooperative pool (a report that
  /// arrives as a `Task`, an abandoned rebuild waking) every chance to run
  /// before asserting that nothing further happened. No wall time enters.
  func settle() async {
    for _ in 0..<1_000 { await Task.yield() }
  }

  /// Collects notices as they arrive, for tests that assert on their absence.
  actor NoticeLog {
    var entries: [CaptureNotice] = []
    func append(_ notice: CaptureNotice) { entries.append(notice) }
  }

  /// What a restarted backend reports: a Bluetooth output's latencies.
  static let restartedStream = CaptureStream(
    sampleRate: StenoAudio.sampleRate, inputLatencyFrames: 480, outputLatencyFrames: 9_600,
    layout: nil)

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

  /// A device that changes and never comes back: four restarts fail across
  /// the backoff ladder and the recording ends in `.deviceLost`, finalised
  /// and readable to its last frame.
  @Test func deviceLostStopsCleanlyWithAReadableMaster() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let clock = ManualClock()
    let backend = SyntheticCaptureBackend(
      lanes: [.mic, .system], tone: [.mic: 440, .system: 1_000], seconds: 10,
      changeDeviceAfter: 1, restartsThatFail: CaptureSession.restartAttempts)
    let session = try CaptureSession(
      configuration: configuration(.call, in: directory), backend: backend,
      echoCanceller: try PassthroughEchoCanceller(sampleRate: 48_000, frameSize: 480),
      clock: clock)
    let states = await session.states
    var notices = await session.notices.makeAsyncIterator()
    let meetingID = UUID()
    try await session.start(meetingID: meetingID)
    #expect(await notices.next() == .deviceChanged(.defaultInputChanged))
    await advance(clock, throughSleeps: CaptureSession.restartAttempts - 1)
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
    #expect(result.statistics.deviceChanges == 0)
    #expect(result.statistics.gapSeconds == 0)
    #expect(result.statistics.duration == 1)
    let master = try CAFFile.read(result.asset.url)
    #expect(master.channels.count == 2)
    #expect(master.frameCount == 48_000)
    #expect(await session.state == .failed(.deviceLost, recording: result))
    #expect(backend.starts == 1 + CaptureSession.restartAttempts)

    // A failed session restarts; this backend's one change is spent, so the
    // second recording sees no change and no restart.
    try await session.start(meetingID: UUID())
    let second = try await session.stop()
    #expect(await session.state == .idle)
    #expect(backend.starts == 2 + CaptureSession.restartAttempts, "one start, no restart")
    #expect(second.statistics.deviceChanges == 0)
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
    let clock = ManualClock()
    let backend = SyntheticCaptureBackend(
      lanes: [.mic, .system], tone: [.mic: 440, .system: 1_000], seconds: 10,
      changeDeviceAfter: 0.5, restartsThatFail: CaptureSession.restartAttempts)
    let session = try CaptureSession(
      configuration: configuration(.call, in: directory), backend: backend,
      echoCanceller: try PassthroughEchoCanceller(sampleRate: 48_000, frameSize: 480),
      clock: clock)
    let states = await session.states
    var notices = await session.notices.makeAsyncIterator()
    try await session.start(meetingID: UUID())
    #expect(await notices.next() == .deviceChanged(.defaultInputChanged))
    await advance(clock, throughSleeps: CaptureSession.restartAttempts - 1)
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

  /// Frame counts move between rates rounded down: a device's to 48 kHz, and
  /// a microphone on its own clock to the stream's; a rate that could not be
  /// read leaves the count alone.
  @Test func frameCountsAreRescaledBetweenRatesRoundingDown() {
    let handsFree = CaptureStream(
      sampleRate: 24_000, inputLatencyFrames: 0, outputLatencyFrames: 0, layout: nil)
    #expect(handsFree.atOutputRate(240 + 4_800) == 10_080)
    #expect(CaptureStream.synthetic.atOutputRate(5_041) == 5_041)
    // 1 000 * 48 000 / 44 100 = 1 088.4.
    let consumer = CaptureStream(
      sampleRate: 44_100, inputLatencyFrames: 0, outputLatencyFrames: 0, layout: nil)
    #expect(consumer.atOutputRate(1_000) == 1_088)
    // A 48 kHz microphone beside a 24 kHz clock master.
    #expect(CaptureStream.rescaled(481, from: 48_000, to: 24_000) == 240)
    #expect(CaptureStream.rescaled(481, from: 0, to: 24_000) == 481)
    #expect(CaptureStream.rescaled(481, from: .nan, to: 24_000) == 481)
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
    failFinish: Bool, clock: any Clock<Duration> = ContinuousClock()
  ) throws -> CaptureSession {
    try CaptureSession(
      configuration: configuration(.inPerson, in: directory), backend: backend,
      echoCanceller: nil, writerHeadroomFrames: 1_000, clock: clock,
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

  // MARK: - Device changes

  /// A device change rebuilds the backend in place: the state never leaves
  /// `.recording`, the notices say what happened, the master keeps growing on
  /// the same files with every frame of both starts and nothing dropped, the
  /// echo canceller starts cold again, and no silence is needed when the
  /// restart succeeds at once.
  @Test func aDeviceChangeKeepsRecordingOnTheSameFiles() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let clock = ManualClock()
    let backend = SyntheticCaptureBackend(
      lanes: [.mic, .system], tone: [.mic: 440, .system: 1_000], seconds: 2, changeDeviceAfter: 1,
      streamAfterRestart: Self.restartedStream)
    let canceller = try ResetLoggingCanceller(sampleRate: 48_000, frameSize: 480)
    let session = try CaptureSession(
      configuration: configuration(.call, in: directory), backend: backend,
      echoCanceller: canceller, writerHeadroomFrames: 1_000, clock: clock)
    let states = await session.states
    var notices = await session.notices.makeAsyncIterator()
    let meetingID = UUID()
    try await session.start(meetingID: meetingID)
    #expect(await session.stream == .synthetic)

    #expect(await notices.next() == .deviceChanged(.defaultInputChanged))
    #expect(await notices.next() == .deviceResumed(attempt: 1, gapSeconds: 0))
    guard case .recording = await session.state else {
      Issue.record("expected .recording after the rebuild, got \(await session.state)")
      return
    }
    #expect(await session.stream == Self.restartedStream, "the rebuilt backend's stream")
    #expect(backend.starts == 2)
    await backend.waitUntilFinished()
    let result = try await session.stop()
    #expect(await session.state == .idle)

    var idles = 0
    let seen = await collectStates(states) { state in
      if state == .idle { idles += 1 }
      return idles == 2
    }
    #expect(
      seen.map(kind) == ["idle", "starting", "recording", "stopping", "idle"],
      "the state never left .recording during the change")
    #expect(result.statistics.deviceChanges == 1)
    #expect(result.statistics.gapSeconds == 0)
    #expect(result.statistics.droppedFrames == [:])
    #expect(!result.statistics.endedOnDeviceLoss)
    #expect(backend.framesDelivered == 3 * 48_000, "one second, then two")
    let master = try CAFFile.read(result.asset.url)
    #expect(master.channels.count == 2)
    #expect(master.frameCount == backend.framesDelivered, "every frame of both starts")
    #expect(result.statistics.duration == 3)
    let layout = RecordingLayout(audioFolder: directory, meetingID: meetingID)
    #expect(result.asset.url == layout.master(.caf48kFloat32), "the same files")
    #expect(try FileManager.default.contentsOfDirectory(atPath: directory.path).count == 1)
    #expect(canceller.log == ["reset", "process", "reset", "process"], "cold filter after")
    #expect(clock.pendingSleepers == 0)
  }

  /// Keeps every far-end sample it is handed and passes the microphone
  /// through.
  final class FarEndRecorder: EchoCanceller, @unchecked Sendable {
    let sampleRate: Double
    let frameSize: Int
    private let lock = NSLock()
    private var samples: [Float] = []

    init(sampleRate: Double, frameSize: Int) throws {
      self.sampleRate = sampleRate
      self.frameSize = frameSize
    }

    var farEnd: [Float] {
      lock.lock()
      defer { lock.unlock() }
      return samples
    }

    func process(
      nearEnd: UnsafeBufferPointer<Float>, farEnd: UnsafeBufferPointer<Float>,
      out: UnsafeMutableBufferPointer<Float>
    ) {
      lock.lock()
      samples.append(contentsOf: farEnd)
      lock.unlock()
      for index in 0..<min(nearEnd.count, out.count) { out[index] = nearEnd[index] }
    }
  }

  /// A headset in the hands-free profile keeps the aggregate at 24 kHz: the
  /// recording still starts, the master and the sidecars are 48 and 16 kHz
  /// with the tones at their frequencies and levels, and the far end reaches
  /// the canceller delayed by the device latencies in 48 kHz frames.
  @Test func aDeviceAt24KilohertzRecordsTheUsualFiles() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let handsFree = CaptureStream(
      sampleRate: 24_000, inputLatencyFrames: 240, outputLatencyFrames: 4_800, layout: nil)
    let backend = SyntheticCaptureBackend(
      lanes: [.mic, .system], tone: [.mic: 440, .system: 1_000], seconds: 3, stream: handsFree)
    let recorder = try FarEndRecorder(sampleRate: 48_000, frameSize: 480)
    let session = try CaptureSession(
      configuration: configuration(.call, in: directory), backend: backend,
      echoCanceller: recorder, writerHeadroomFrames: 1_000)
    try await session.start(meetingID: UUID())
    #expect(await session.stream?.sampleRate == 24_000)
    await backend.waitUntilFinished()
    let result = try await session.stop()

    #expect(backend.framesDelivered == 3 * 24_000)
    #expect(result.statistics.droppedFrames == [:])
    // 72 000 device samples less half a converter window convert to
    // 2 * (72 000 - 32) = 143 936 outputs: 299 whole frames.
    let master = try CAFFile.read(result.asset.url)
    #expect(master.frameCount == 143_520)
    #expect(result.statistics.duration == 143_520 / StenoAudio.sampleRate)
    #expect(master.sampleRate == StenoAudio.sampleRate)
    #expect(master.channels.count == 2)
    for (channel, hertz) in [(0, 440.0), (1, 1_000.0)] {
      let steady = master.channels[channel][4_800..<140_000]
      let measured = RateConverterTests.frequency(steady)
      #expect(abs(measured - hertz) < 2, "\(measured) Hz")
      #expect(abs(RateConverterTests.levelAgainstHalfScaleSine(steady)) < 0.1)
    }
    let mic = try WAVAudioDecoder.read(result.asset.sidecars16k[.mic]!)
    #expect(mic.samples.count == 143_520 / 3)
    // 240 + 4 800 frames at 24 kHz are 10 080 at 48 kHz: the far end is the
    // system lane that many samples late, zeros before.
    let farEnd = recorder.farEnd
    let system = master.channels[1]
    #expect(farEnd.count == system.count)
    #expect(farEnd.prefix(10_080).allSatisfy { $0 == 0 })
    #expect(Array(farEnd.dropFirst(10_080)) == Array(system.prefix(system.count - 10_080)))
  }

  /// The call starts and the headset enters the hands-free profile while
  /// recording: the rebuilt backend comes back at 24 kHz and the recording
  /// carries on instead of ending as a lost device.
  @Test func aChangeTo24KilohertzResumesTheRecording() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let clock = ManualClock()
    let handsFree = CaptureStream(
      sampleRate: 24_000, inputLatencyFrames: 240, outputLatencyFrames: 4_800, layout: nil)
    let backend = SyntheticCaptureBackend(
      lanes: [.mic, .system], tone: [.mic: 440, .system: 1_000], seconds: 2, changeDeviceAfter: 1,
      streamAfterRestart: handsFree)
    let session = try CaptureSession(
      configuration: configuration(.call, in: directory), backend: backend,
      echoCanceller: try PassthroughEchoCanceller(sampleRate: 48_000, frameSize: 480),
      writerHeadroomFrames: 1_000, clock: clock)
    var notices = await session.notices.makeAsyncIterator()
    try await session.start(meetingID: UUID())
    #expect(await notices.next() == .deviceChanged(.defaultInputChanged))
    #expect(await notices.next() == .deviceResumed(attempt: 1, gapSeconds: 0))
    #expect(await session.stream == handsFree)
    await backend.waitUntilFinished()
    let result = try await session.stop()

    #expect(backend.framesDelivered == 48_000 + 2 * 24_000)
    #expect(result.statistics.deviceChanges == 1)
    #expect(!result.statistics.endedOnDeviceLoss)
    #expect(result.statistics.droppedFrames == [:])
    // 100 frames at 48 kHz, then 48 000 samples at 24 kHz less half a window:
    // 2 * (48 000 - 32) = 95 936 outputs, 199 whole frames.
    let master = try CAFFile.read(result.asset.url)
    #expect(master.frameCount == (100 + 199) * 480)
    let after = master.channels[0][52_800..<140_000]
    let measured = RateConverterTests.frequency(after)
    #expect(abs(measured - 440) < 2, "\(measured) Hz after the change")
  }

  /// The contiguity claim. A gap longer than the two seconds the sink's
  /// rings hold is written in full as silence through the relay, so the
  /// master runs to wall time with nothing truncated into `droppedFrames`:
  /// three restarts fail, the clock advances 0.25, 0.5 and then 2.5 s (a
  /// sleeper wakes at its deadline however far the clock moves past it), the
  /// fourth succeeds, and 3.25 s of zeros sit between the old device's last
  /// frame and the new device's first. A gap pushed through the rings would
  /// lose 1.25 s of it; this is the test that fails then. The state stays
  /// `.recording` throughout.
  @Test func aGapLongerThanTheRingIsWrittenInFull() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let clock = ManualClock()
    let backend = SyntheticCaptureBackend(
      lanes: [.mic, .system], tone: [.mic: 440, .system: 1_000], seconds: 2, changeDeviceAfter: 1,
      restartsThatFail: 3)
    let session = try CaptureSession(
      configuration: configuration(.call, in: directory), backend: backend,
      echoCanceller: try PassthroughEchoCanceller(sampleRate: 48_000, frameSize: 480),
      writerHeadroomFrames: 1_000, clock: clock)
    let states = await session.states
    var notices = await session.notices.makeAsyncIterator()
    try await session.start(meetingID: UUID())
    #expect(await notices.next() == .deviceChanged(.defaultInputChanged))
    await advance(clock, throughSleeps: 2)
    #expect(await clock.waitForSleepers(1), "the third backoff")
    clock.advance(by: .milliseconds(2_500))
    #expect(await notices.next() == .deviceResumed(attempt: 4, gapSeconds: 3.25))
    guard case .recording = await session.state else {
      Issue.record("expected .recording after the rebuild, got \(await session.state)")
      return
    }
    await backend.waitUntilFinished()
    let result = try await session.stop()

    var idles = 0
    let seen = await collectStates(states) { state in
      if state == .idle { idles += 1 }
      return idles == 2
    }
    #expect(seen.map(kind) == ["idle", "starting", "recording", "stopping", "idle"])
    #expect(result.statistics.gapSeconds == 3.25)
    #expect(result.statistics.deviceChanges == 1)
    #expect(result.statistics.droppedFrames == [:])
    #expect(!result.statistics.endedOnDeviceLoss)
    #expect(backend.starts == 5)
    let oldFrames = 48_000
    let gapFrames = Int(3.25 * 48_000)
    #expect(backend.framesDelivered == 3 * 48_000)
    let master = try CAFFile.read(result.asset.url)
    #expect(master.frameCount == backend.framesDelivered + gapFrames)
    #expect(result.statistics.duration == 6.25)
    for channel in master.channels {
      #expect(
        channel[(oldFrames - 480)..<oldFrames].contains { $0 != 0 },
        "the old device's last frame precedes the gap")
      #expect(
        channel[oldFrames..<(oldFrames + gapFrames)].allSatisfy { $0 == 0 }, "the gap is silence")
      #expect(
        channel[(oldFrames + gapFrames)..<(oldFrames + gapFrames + 480)].contains { $0 != 0 },
        "the new device's first frame follows it")
    }
    let sidecar = try WAVAudioDecoder.read(result.asset.sidecars16k[.mic]!)
    #expect(sidecar.samples.count == master.frameCount / 3, "the sidecars carry the gap too")
  }

  /// The shape a Bluetooth headset produces (out of the profile and back):
  /// two changes, two rebuilds, four notices in order, one master.
  @Test func twoDeviceChangesRebuildTwice() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let clock = ManualClock()
    let backend = SyntheticCaptureBackend(
      lanes: [.mic, .system], tone: [.mic: 440, .system: 1_000], seconds: 2,
      changeDeviceAfter: 0.5, changes: 2)
    let session = try CaptureSession(
      configuration: configuration(.call, in: directory), backend: backend,
      echoCanceller: try PassthroughEchoCanceller(sampleRate: 48_000, frameSize: 480),
      writerHeadroomFrames: 1_000, clock: clock)
    var notices = await session.notices.makeAsyncIterator()
    try await session.start(meetingID: UUID())
    var seen: [CaptureNotice] = []
    for _ in 0..<4 {
      guard let notice = await notices.next() else { break }
      seen.append(notice)
    }
    #expect(
      seen == [
        .deviceChanged(.defaultInputChanged), .deviceResumed(attempt: 1, gapSeconds: 0),
        .deviceChanged(.defaultInputChanged), .deviceResumed(attempt: 1, gapSeconds: 0),
      ])
    await backend.waitUntilFinished()
    let result = try await session.stop()
    #expect(result.statistics.deviceChanges == 2)
    #expect(result.statistics.gapSeconds == 0)
    #expect(result.statistics.droppedFrames == [:])
    #expect(!result.statistics.endedOnDeviceLoss)
    #expect(backend.starts == 3)
    #expect(backend.framesDelivered == 24_000 + 24_000 + 96_000)
    #expect(try CAFFile.read(result.asset.url).frameCount == backend.framesDelivered)
  }

  /// Four failed restarts end the recording in `.deviceLost` after the
  /// summed backoff on the manual clock, with what was recorded before the
  /// change and nothing rebuilt.
  @Test func aRestartThatKeepsFailingEndsInDeviceLost() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let clock = ManualClock()
    let backend = SyntheticCaptureBackend(
      lanes: [.mic, .system], tone: [.mic: 440, .system: 1_000], seconds: 10,
      changeDeviceAfter: 0.5, restartsThatFail: 4)
    let session = try CaptureSession(
      configuration: configuration(.call, in: directory), backend: backend,
      echoCanceller: try PassthroughEchoCanceller(sampleRate: 48_000, frameSize: 480),
      clock: clock)
    let states = await session.states
    let log = NoticeLog()
    let noticeStream = await session.notices
    let collector = Task { for await notice in noticeStream { await log.append(notice) } }
    try await session.start(meetingID: UUID())
    // Three sleeps separate the four attempts; none before the first.
    await advance(clock, throughSleeps: 3)
    #expect(clock.now.offset == .milliseconds(1_750))
    let seen = await collectStates(states) { state in
      if case .failed = state { return true }
      return false
    }
    #expect(seen.last?.failure == .deviceLost)
    let result = try await session.stop()
    #expect(seen.last == .failed(.deviceLost, recording: result))
    #expect(result.statistics.endedOnDeviceLoss)
    #expect(result.statistics.deviceChanges == 0)
    #expect(result.statistics.gapSeconds == 0)
    #expect(result.statistics.duration == 0.5)
    #expect(try CAFFile.read(result.asset.url).frameCount == 24_000)
    #expect(backend.starts == 5, "one start and four failed restarts")
    #expect(clock.pendingSleepers == 0)
    await settle()
    #expect(await log.entries == [.deviceChanged(.defaultInputChanged)], "no resumed notice")
    collector.cancel()
  }

  /// A backend whose `stop()` reports a change on the sink it was given, the
  /// way a HAL listener can fire while the session tears down.
  final class ReportingOnStop: CaptureBackend, @unchecked Sendable {
    let inner: SyntheticCaptureBackend
    private let lock = NSLock()
    private var sink: LaneFrameSink?

    init(_ inner: SyntheticCaptureBackend) { self.inner = inner }

    func start(lanes: [AudioLane], inputDeviceUID: String?, sink: LaneFrameSink) throws
      -> CaptureStream
    {
      lock.lock()
      self.sink = sink
      lock.unlock()
      return try inner.start(lanes: lanes, inputDeviceUID: inputDeviceUID, sink: sink)
    }

    func stop() {
      inner.stop()
      lock.lock()
      let sink = self.sink
      lock.unlock()
      sink?.reportDeviceChange(.outputDeviceGone)
    }
  }

  /// A report before `start` and one from inside `stop()` produce no notice
  /// and leave the state as it was. The second reaches the actor as a task
  /// once `stop()` has returned, so the guard sees `.idle` both times; the
  /// same `case .recording` guard drops a report during `.stopping` and
  /// `.starting`.
  @Test func aDeviceChangeWhileIdleOrAfterStopIsIgnored() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let backend = ReportingOnStop(
      SyntheticCaptureBackend(lanes: [.mixed], tone: [.mixed: 440], seconds: 0.2))
    let session = try CaptureSession(
      configuration: configuration(.inPerson, in: directory), backend: backend)
    let log = NoticeLog()
    let noticeStream = await session.notices
    let collector = Task { for await notice in noticeStream { await log.append(notice) } }

    await session.deviceChanged(.defaultInputChanged)
    #expect(await session.state == .idle)

    try await session.start(meetingID: UUID())
    await backend.inner.waitUntilFinished()
    let result = try await session.stop()
    #expect(await session.state == .idle)
    await settle()
    #expect(await session.state == .idle)
    #expect(await log.entries.isEmpty)
    #expect(result.statistics.deviceChanges == 0)
    #expect(!result.statistics.endedOnDeviceLoss)
    #expect(backend.inner.starts == 1, "nothing was restarted")
    collector.cancel()
  }

  /// The writer fails while a rebuild is under way (on the first frame of
  /// gap silence, the 51st frame written): the session ends
  /// `.failed(.writerFailed)` with what was written, not `.deviceLost`, and
  /// the rebuild does not resurrect it.
  @Test func aWriterFailureDuringARebuildEndsWriterFailed() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let clock = ManualClock()
    let backend = SyntheticCaptureBackend(
      lanes: [.mixed], tone: [.mixed: 440], seconds: 2, changeDeviceAfter: 0.5, restartsThatFail: 1
    )
    let session = try faultySession(
      in: directory, backend: backend, failAfterFrames: 50, failFinish: false, clock: clock)
    let states = await session.states
    var notices = await session.notices.makeAsyncIterator()
    try await session.start(meetingID: UUID())
    #expect(await notices.next() == .deviceChanged(.defaultInputChanged))
    // One failed restart, 250 ms on the clock, then 25 frames of silence.
    await advance(clock, throughSleeps: 1)
    let seen = await collectStates(states) { state in
      if case .failed = state { return true }
      return false
    }
    guard case .failed(.writerFailed(let detail), let recording) = seen.last else {
      Issue.record("expected .failed(.writerFailed), got \(String(describing: seen.last))")
      return
    }
    #expect(detail.contains("DiskFull"))
    let result = try await session.stop()
    #expect(recording == result)
    #expect(!result.statistics.endedOnDeviceLoss)
    #expect(result.statistics.duration == 0.5)
    #expect(try CAFFile.read(result.asset.url).frameCount == 50 * 480)
    await settle()
    #expect(await session.state == .failed(.writerFailed(detail), recording: result))
    #expect(clock.pendingSleepers == 0)
  }

  /// `stop()` while the rebuild waits out a backoff abandons it: the
  /// recording is finalised once, ends `.idle`, the cancelled sleep is gone,
  /// and nothing the clock does afterwards changes the outcome.
  @Test func stopDuringARebuildFinalisesOnce() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let clock = ManualClock()
    let backend = SyntheticCaptureBackend(
      lanes: [.mic, .system], tone: [.mic: 440, .system: 1_000], seconds: 10,
      changeDeviceAfter: 0.5, restartsThatFail: 4)
    let session = try CaptureSession(
      configuration: configuration(.call, in: directory), backend: backend,
      echoCanceller: try PassthroughEchoCanceller(sampleRate: 48_000, frameSize: 480),
      clock: clock)
    let states = await session.states
    var notices = await session.notices.makeAsyncIterator()
    let meetingID = UUID()
    try await session.start(meetingID: meetingID)
    #expect(await notices.next() == .deviceChanged(.defaultInputChanged))
    #expect(await clock.waitForSleepers(1), "the rebuild is waiting out the first backoff")

    let result = try await session.stop()
    #expect(await session.state == .idle)
    #expect(clock.pendingSleepers == 0, "the abandoned rebuild's sleep was cancelled")
    #expect(!result.statistics.endedOnDeviceLoss)
    #expect(result.statistics.deviceChanges == 0)
    #expect(result.statistics.gapSeconds == 0)
    #expect(result.statistics.duration == 0.5)
    #expect(try CAFFile.read(result.asset.url).frameCount == 24_000)
    #expect(backend.starts == 2, "one start, one failed restart")

    clock.advance(by: .seconds(10))
    await settle()
    #expect(await session.state == .idle)
    #expect(backend.starts == 2, "nothing after the stop")
    await #expect(throws: CaptureError.self) { try await session.stop() }
    var idles = 0
    let seen = await collectStates(states) { state in
      if state == .idle { idles += 1 }
      return idles == 2
    }
    #expect(seen.map(kind) == ["idle", "starting", "recording", "stopping", "idle"])
    #expect(try FileManager.default.contentsOfDirectory(atPath: directory.path).count == 1)
  }

  /// Whole relay frames for a gap, rounded down: what `gapSeconds` reports.
  @Test func gapFramesRoundDownToWholeFrames() {
    #expect(CaptureSession.gapFrames(for: .zero) == 0)
    #expect(CaptureSession.gapFrames(for: .milliseconds(255)) == 25)
    #expect(CaptureSession.gapFrames(for: .seconds(10)) == 1_000)
  }

  /// In production the relay holds two seconds; a gap wider than that waits
  /// for the writer in 5 ms steps on the clock, and every frame of silence
  /// still arrives: 1.75 s of gap through a one-second relay. Both recorded
  /// stretches fit the relay on their own, so nothing else can drop.
  @Test func aGapWiderThanTheRelayWaitsForTheWriterAndLosesNothing() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let clock = ManualClock()
    let backend = SyntheticCaptureBackend(
      lanes: [.mic, .system], tone: [.mic: 440, .system: 1_000], seconds: 1,
      changeDeviceAfter: 0.5, restartsThatFail: 3)
    let session = try CaptureSession(
      configuration: configuration(.call, in: directory), backend: backend,
      echoCanceller: try PassthroughEchoCanceller(sampleRate: 48_000, frameSize: 480),
      writerHeadroomFrames: 100, clock: clock)
    let log = NoticeLog()
    let noticeStream = await session.notices
    let collector = Task { for await notice in noticeStream { await log.append(notice) } }
    try await session.start(meetingID: UUID())
    await advance(clock, throughSleeps: 3)
    // The rebuilt backend delivers its second at machine speed into the rings
    // while the gap waits on the clock; let it finish first, so the backlog
    // the session waits for room for is the whole second.
    while backend.starts < 5 { await Task.yield() }
    await backend.waitUntilFinished()
    await driveRelayWaits(clock) { await log.entries.count == 2 }
    #expect(
      await log.entries == [
        .deviceChanged(.defaultInputChanged), .deviceResumed(attempt: 4, gapSeconds: 1.75),
      ])
    await backend.waitUntilFinished()
    let result = try await session.stop()
    collector.cancel()

    #expect(result.statistics.gapSeconds == 1.75)
    #expect(result.statistics.deviceChanges == 1)
    #expect(result.statistics.droppedFrames == [:])
    #expect(backend.framesDelivered == 24_000 + 48_000)
    #expect(try CAFFile.read(result.asset.url).frameCount == backend.framesDelivered + 84_000)
    #expect(result.statistics.duration == 3.25)
    #expect(clock.pendingSleepers == 0)
  }

  /// `maximumGap`: an outage of 30 s on the clock fills 10 s of silence, and
  /// `gapSeconds` reports the capped value the master actually holds.
  @Test func aGapIsCappedAtTenSeconds() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let clock = ManualClock()
    let backend = SyntheticCaptureBackend(
      lanes: [.mic, .system], tone: [.mic: 440, .system: 1_000], seconds: 2, changeDeviceAfter: 1,
      restartsThatFail: 1)
    let session = try CaptureSession(
      configuration: configuration(.call, in: directory), backend: backend,
      echoCanceller: try PassthroughEchoCanceller(sampleRate: 48_000, frameSize: 480),
      writerHeadroomFrames: 2_000, clock: clock)
    var notices = await session.notices.makeAsyncIterator()
    try await session.start(meetingID: UUID())
    #expect(await notices.next() == .deviceChanged(.defaultInputChanged))
    #expect(await clock.waitForSleepers(1))
    clock.advance(by: .seconds(30))
    #expect(await notices.next() == .deviceResumed(attempt: 2, gapSeconds: 10))
    await backend.waitUntilFinished()
    let result = try await session.stop()
    #expect(result.statistics.gapSeconds == 10)
    #expect(result.statistics.droppedFrames == [:])
    #expect(try CAFFile.read(result.asset.url).frameCount == backend.framesDelivered + 480_000)
    #expect(result.statistics.duration == 13)
  }

  /// `stop()` while the rebuild waits for the writer to drain the relay
  /// abandons it: the recording is finalised once, ends `.idle`, the 5 ms
  /// sleep is cancelled, and the gap counts for nothing because it was never
  /// completed. The master holds the old device's half second and whatever
  /// silence reached the relay before the stop; the rebuilt backend's frames
  /// never left the rings.
  @Test func stopDuringTheGapWriteFinalisesOnce() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let clock = ManualClock()
    let backend = SyntheticCaptureBackend(
      lanes: [.mic, .system], tone: [.mic: 440, .system: 1_000], seconds: 1,
      changeDeviceAfter: 0.5, restartsThatFail: 3)
    let session = try CaptureSession(
      configuration: configuration(.call, in: directory), backend: backend,
      echoCanceller: try PassthroughEchoCanceller(sampleRate: 48_000, frameSize: 480),
      writerHeadroomFrames: 100, clock: clock)
    var notices = await session.notices.makeAsyncIterator()
    try await session.start(meetingID: UUID())
    #expect(await notices.next() == .deviceChanged(.defaultInputChanged))
    await advance(clock, throughSleeps: 3)
    // 175 frames of silence through a relay of 100: the writer cannot keep
    // pace with the actor's zeros, so the rebuild sleeps on the clock.
    #expect(await clock.waitForSleepers(1), "the rebuild waits for the writer")
    #expect(backend.starts == 5)

    let result = try await session.stop()
    #expect(await session.state == .idle)
    #expect(clock.pendingSleepers == 0, "the abandoned wait was cancelled")
    #expect(result.statistics.deviceChanges == 0)
    #expect(result.statistics.gapSeconds == 0)
    #expect(result.statistics.droppedFrames == [:])
    #expect(!result.statistics.endedOnDeviceLoss)
    let frames = try CAFFile.read(result.asset.url).frameCount
    #expect(frames >= 24_000 && frames <= 24_000 + 175 * 480, "old audio plus partial silence")
    await settle()
    #expect(await session.state == .idle)
    #expect(backend.starts == 5, "nothing after the stop")
  }

  /// A change reported while a rebuild is under way (here from the test; in
  /// production from the rebuilt backend's listeners before the gap is
  /// written) is not lost: `resume` starts the next rebuild from it, so the
  /// notices come in pairs and the second rebuild runs at once.
  @Test func aChangeReportedDuringARebuildStartsTheNextOne() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    let clock = ManualClock()
    let backend = SyntheticCaptureBackend(
      lanes: [.mic, .system], tone: [.mic: 440, .system: 1_000], seconds: 2,
      changeDeviceAfter: 0.5, restartsThatFail: 1)
    let session = try CaptureSession(
      configuration: configuration(.call, in: directory), backend: backend,
      echoCanceller: try PassthroughEchoCanceller(sampleRate: 48_000, frameSize: 480),
      writerHeadroomFrames: 1_000, clock: clock)
    var notices = await session.notices.makeAsyncIterator()
    try await session.start(meetingID: UUID())
    #expect(await notices.next() == .deviceChanged(.defaultInputChanged))
    #expect(await clock.waitForSleepers(1), "the first restart failed; the rebuild waits")
    await session.deviceChanged(.outputDeviceGone)
    clock.advance(by: .milliseconds(250))
    #expect(await notices.next() == .deviceResumed(attempt: 2, gapSeconds: 0.25))
    #expect(await notices.next() == .deviceChanged(.outputDeviceGone), "kept, not dropped")
    #expect(await notices.next() == .deviceResumed(attempt: 1, gapSeconds: 0))
    await backend.waitUntilFinished()
    let result = try await session.stop()
    #expect(result.statistics.deviceChanges == 2)
    #expect(result.statistics.gapSeconds == 0.25)
    #expect(result.statistics.droppedFrames == [:])
    #expect(backend.starts == 4, "start, failed restart, restart, restart")
    // The second rebuild stopped its backend mid-callback, so only the
    // sub-frame residue is missing from the master.
    let frames = try CAFFile.read(result.asset.url).frameCount
    let expected = backend.framesDelivered + 12_000
    #expect(frames <= expected && frames > expected - 480)
    #expect(clock.pendingSleepers == 0)
  }
}
