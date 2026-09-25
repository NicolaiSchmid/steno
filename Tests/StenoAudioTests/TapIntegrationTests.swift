import Foundation
import StenoCore
import Testing

@testable import StenoAudio

/// Real devices, opt-in: `STENO_AUDIO_TESTS=1` starts the live tap while
/// `afplay` (a separate process, so not excluded) plays a 1 kHz tone and
/// asserts the tone reaches the system lane. The mic-lane variant needs a
/// virtual input device and is gated on `STENO_VIRTUAL_INPUT_UID`. Run from
/// a context that already holds the system-audio permission.
@Suite(.serialized) struct TapIntegrationTests {
  static let environment = ProcessInfo.processInfo.environment
  static let enabled = environment["STENO_AUDIO_TESTS"] == "1"
  static let virtualInput = environment["STENO_VIRTUAL_INPUT_UID"]

  #if canImport(CoreAudio)
    /// Records `lanes` for `seconds` while a tone plays through the default
    /// output; returns the master's channels.
    static func recordWithTone(lanes: [AudioLane], inputDeviceUID: String?, seconds: Double)
      async throws -> [[Float]]
    {
      let directory = try Fixtures.temporaryDirectory("tap")
      defer { try? FileManager.default.removeItem(at: directory) }
      let tone = directory.appendingPathComponent("tone-1k-48k.wav")
      try AudioFixtures.writeWAV(
        AudioFixtures.tone(frequency: 1_000, seconds: seconds + 1, amplitude: 0.5), to: tone)
      let player = Foundation.Process()
      player.executableURL = URL(fileURLWithPath: "/usr/bin/afplay")
      player.arguments = [tone.path]
      try player.run()
      defer { if player.isRunning { player.terminate() } }

      let session = try CaptureSession(
        configuration: CaptureConfiguration(
          mode: .call, inputDeviceUID: inputDeviceUID, echoCancellation: false,
          outputDirectory: directory, laneOverride: lanes),
        backend: LiveCaptureBackend())
      try await session.start(meetingID: UUID())
      try await Task.sleep(for: .milliseconds(Int(seconds * 1_000)))
      let result = try await session.stop()
      #expect(result.statistics.droppedFrames == [:])
      return try CAFFile.read(result.asset.url).channels
    }
  #endif

  @Test(.enabled(if: enabled, "Set STENO_AUDIO_TESTS=1 to run against real devices"))
  func systemLaneCarriesTheToneAfplayPlays() async throws {
    #if canImport(CoreAudio)
      let channels = try await Self.recordWithTone(
        lanes: [.system], inputDeviceUID: nil, seconds: 3)
      let system = channels[0]
      let steady = system[(48_000)...]
      let level = EchoMetrics.decibels(EchoMetrics.rms(steady))
      let tone = EchoMetrics.toneLevel(steady, frequency: 1_000, sampleRate: 48_000)
      let peak = EchoMetrics.decibels(tone)
      #expect(level > -30, "system lane at \(level) dBFS")
      #expect(peak > level - 3, "1 kHz dominates: \(peak) dBFS of \(level) dBFS")
    #endif
  }

  @Test(
    .enabled(
      if: enabled && virtualInput != nil,
      "Set STENO_AUDIO_TESTS=1 and STENO_VIRTUAL_INPUT_UID to a virtual input device"))
  func micLaneJoinsTheAggregate() async throws {
    #if canImport(CoreAudio)
      let channels = try await Self.recordWithTone(
        lanes: [.mic, .system], inputDeviceUID: Self.virtualInput, seconds: 3)
      #expect(channels.count == 2)
      #expect(channels[0].count == channels[1].count)
      #expect(channels[0].contains { $0 != 0 }, "the virtual input delivered samples")
    #endif
  }
}
