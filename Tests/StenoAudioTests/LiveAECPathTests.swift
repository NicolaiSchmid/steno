import Foundation
import StenoCore
import Testing

@testable import StenoAudio

/// Echo cancellation in the real processing path: synthetic backend → rings →
/// ProcessingThread with SpeexEchoCanceller → relay → RecordingWriter, then
/// the files are measured. The far-end is a sine on the system lane; the mic
/// hears it 60 ms later at -6 dB, plus an independent tone in the second
/// test.
@Suite(.timeLimit(.minutes(2))) struct LiveAECPathTests {
  static let farFrequency = 1_000.0
  static let ownFrequency = 320.0

  func record(
    signals: [AudioLane: SyntheticLane], seconds: Double, keepRaw: Bool = true
  ) async throws -> (asset: AudioAsset, raw: [Float], processed: [Float], system: [Float]) {
    let directory = try Fixtures.temporaryDirectory("aec-path")
    defer { try? FileManager.default.removeItem(at: directory) }
    let backend = SyntheticCaptureBackend(signals: signals, seconds: seconds)
    let session = try CaptureSession(
      configuration: CaptureConfiguration(
        mode: .call, echoCancellation: true, keepRawMicLane: keepRaw, outputDirectory: directory),
      backend: backend, writerHeadroomFrames: 1_500)
    try await session.start(meetingID: UUID())
    await backend.waitUntilFinished()
    let result = try await session.stop()
    #expect(result.statistics.droppedFrames == [:])
    let master = try CAFFile.read(result.asset.url)
    let raw = try CAFFile.read(
      RecordingLayout(asset: result.asset).directory.appendingPathComponent("mic.raw.caf"))
    return (result.asset, raw.channels[0], master.channels[0], master.channels[1])
  }

  @Test func echoOfTheSystemLaneIsCancelledOnTheMicLane() async throws {
    let recording = try await record(
      signals: [
        .mic: SyntheticLane(
          frequency: 0, amplitude: 0, echo: .init(of: .system, delay: 0.060, gain: 0.5)),
        .system: SyntheticLane(frequency: Self.farFrequency, amplitude: 0.5),
      ], seconds: 4)
    let range = (2 * 48_000)..<(4 * 48_000)
    let erle = EchoMetrics.erle(
      nearEnd: recording.raw, processed: recording.processed, range: range)
    #expect(erle >= 15, "ERLE \(erle) dB over the last two seconds")
    #expect(recording.raw.count == recording.processed.count)
    #expect(abs(EchoMetrics.decibels(EchoMetrics.rms(recording.raw[range])) - -15.05) < 0.2)
    #expect(abs(EchoMetrics.decibels(EchoMetrics.rms(recording.system[range])) - -9.03) < 0.2)
  }

  @Test func theIndependentToneSurvivesWithinThreeDecibels() async throws {
    let recording = try await record(
      signals: [
        .mic: SyntheticLane(
          frequency: Self.ownFrequency, amplitude: 0.25,
          echo: .init(of: .system, delay: 0.060, gain: 0.5)),
        .system: SyntheticLane(frequency: Self.farFrequency, amplitude: 0.5),
      ], seconds: 4)
    let range = (2 * 48_000)..<(4 * 48_000)
    let before = EchoMetrics.toneLevel(
      recording.raw[range], frequency: Self.ownFrequency, sampleRate: 48_000)
    let after = EchoMetrics.toneLevel(
      recording.processed[range], frequency: Self.ownFrequency, sampleRate: 48_000)
    let change = 20 * log10(after / before)
    #expect(abs(change) <= 3, "own tone changed by \(change) dB")
    #expect(abs(before - 0.25) < 0.01)

    let echoBefore = EchoMetrics.toneLevel(
      recording.raw[range], frequency: Self.farFrequency, sampleRate: 48_000)
    let echoAfter = EchoMetrics.toneLevel(
      recording.processed[range], frequency: Self.farFrequency, sampleRate: 48_000)
    let erle = 20 * log10(echoBefore / echoAfter)
    #expect(erle >= 15, "echo tone ERLE \(erle) dB under double talk")
  }

  @Test func goertzelReadsAFullScaleSineAsOne() {
    let tone = AudioFixtures.tone(frequency: 1_000, seconds: 1, amplitude: 1)
    #expect(abs(EchoMetrics.toneLevel(tone[...], frequency: 1_000, sampleRate: 48_000) - 1) < 0.01)
    #expect(EchoMetrics.toneLevel(tone[...], frequency: 3_000, sampleRate: 48_000) < 0.01)
  }
}
