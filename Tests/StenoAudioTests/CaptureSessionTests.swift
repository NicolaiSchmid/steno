import Foundation
import StenoCore
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
    #expect(result.statistics.deviceChanges == 0)

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
    #expect(seen.last == .failed(.deviceLost))
    #expect(seen.contains(.stopping))

    let result = try await session.stop()
    #expect(result.statistics.deviceChanges == 1)
    #expect(abs(result.statistics.duration - 1) < 0.05)
    let master = try CAFFile.read(result.asset.url)
    #expect(master.channels.count == 2)
    #expect(abs(master.duration - 1) < 0.05)
    #expect(await session.state == .failed(.deviceLost))

    // A failed session restarts (and this backend loses its device again).
    try await session.start(meetingID: UUID())
    _ = try await session.stop()
    let restarted = await session.state
    #expect(restarted == .idle || restarted == .failed(.deviceLost))
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
    #expect(first.asset == second.asset)
    #expect(first.statistics == second.statistics)
    #expect(await session.state == .failed(.deviceLost))
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

  @Test func aFailingBackendLeavesTheSessionFailedAndNoFolder() async throws {
    let directory = try Fixtures.temporaryDirectory("session")
    defer { try? FileManager.default.removeItem(at: directory) }
    struct Failing: CaptureBackend {
      func start(lanes: [AudioLane], inputDeviceUID: String?, sink: LaneFrameSink) throws {
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
    #expect(await session.state == .failed(.inputDeviceUnavailable))
    let layout = RecordingLayout(audioFolder: directory, meetingID: meetingID)
    #expect(!FileManager.default.fileExists(atPath: layout.directory.path))
    await #expect(throws: CaptureError.self) { try await session.stop() }
  }
}
