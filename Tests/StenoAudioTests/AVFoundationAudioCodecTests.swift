import Foundation
import StenoCore
import Testing

@testable import StenoAudio

#if canImport(AVFoundation)
  import AVFoundation

  /// Decode and mixdown on files built in setup: a two-channel 48 kHz CAF from
  /// the recording writer and a phone-style AAC `.m4a`. Never committed: AAC
  /// is not byte-stable.
  @Suite struct AVFoundationAudioCodecTests {
    static func rms(_ samples: ArraySlice<Float>) -> Float { EchoMetrics.rms(samples) }

    /// A two-lane meeting folder: 1 kHz on the mic lane at 0.5, 1 kHz on the
    /// system lane at 0.25, two seconds, with sidecars.
    static func makeCallAsset(in directory: URL) throws -> AudioAsset {
      let meetingID = UUID()
      let layout = RecordingLayout(audioFolder: directory, meetingID: meetingID)
      let writer = try RecordingWriter(layout: layout, lanes: [.mic, .system])
      let mic = AudioFixtures.tone(frequency: 1_000, seconds: 2, amplitude: 0.5)
      let system = AudioFixtures.tone(frequency: 1_000, seconds: 2, amplitude: 0.25)
      for start in stride(from: 0, to: mic.count, by: 480) {
        try mic.withUnsafeBufferPointer { m in
          try system.withUnsafeBufferPointer { s in
            try writer.write(
              LaneFrames(frameCount: 480, lanes: [m.baseAddress! + start, s.baseAddress! + start]))
          }
        }
      }
      let files = try writer.finish()
      return AudioAsset(
        id: UUID(), meetingID: meetingID, url: files.master, format: .caf48kFloat32,
        lanes: [.mic, .system], sidecars16k: files.sidecars16k, retention: .keepForever)
    }

    /// A phone recording: AAC mono 48 kHz, 1 kHz at 0.5 for two seconds.
    static func makePhoneAsset(in directory: URL) throws -> AudioAsset {
      let meetingID = UUID()
      let layout = RecordingLayout(audioFolder: directory, meetingID: meetingID)
      try layout.createDirectories()
      let url = layout.master(.m4aAAC)
      let format = AVAudioFormat(standardFormatWithSampleRate: 48_000, channels: 1)!
      let file = try AVAudioFile(
        forWriting: url,
        settings: [
          AVFormatIDKey: kAudioFormatMPEG4AAC, AVSampleRateKey: 48_000.0,
          AVNumberOfChannelsKey: 1, AVEncoderBitRateKey: 64_000,
        ], commonFormat: .pcmFormatFloat32, interleaved: false)
      let tone = AudioFixtures.tone(frequency: 1_000, seconds: 2, amplitude: 0.5)
      let buffer = AVAudioPCMBuffer(
        pcmFormat: format, frameCapacity: AVAudioFrameCount(tone.count))!
      buffer.frameLength = AVAudioFrameCount(tone.count)
      tone.withUnsafeBufferPointer {
        buffer.floatChannelData![0].update(from: $0.baseAddress!, count: tone.count)
      }
      try file.write(from: buffer)
      return AudioAsset(
        id: UUID(), meetingID: meetingID, url: url, format: .m4aAAC, lanes: [.mixed],
        retention: .keepForever)
    }

    @Test func decodesEachLaneOfTheMasterLikeItsSidecar() async throws {
      let directory = try Fixtures.temporaryDirectory("codec")
      defer { try? FileManager.default.removeItem(at: directory) }
      let asset = try Self.makeCallAsset(in: directory)
      let codec = AVFoundationAudioCodec()
      #expect(codec.mixdownFormat == .m4aAAC)

      let micFromSidecar = try await codec.decode(asset, lane: .mic)
      let systemFromSidecar = try await codec.decode(asset, lane: .system)
      var withoutSidecars = asset
      withoutSidecars.sidecars16k = [:]
      let micFromMaster = try await codec.decode(withoutSidecars, lane: .mic)
      let systemFromMaster = try await codec.decode(withoutSidecars, lane: .system)

      #expect(micFromSidecar.samples.count == 32_000)
      #expect(abs(micFromMaster.samples.count - 32_000) <= 64, "\(micFromMaster.samples.count)")
      #expect(abs(systemFromMaster.samples.count - 32_000) <= 64)
      let window = 4_000..<30_000
      let micDifference =
        20
        * log10(Self.rms(micFromMaster.samples[window]) / Self.rms(micFromSidecar.samples[window]))
      let systemDifference =
        20
        * log10(
          Self.rms(systemFromMaster.samples[window]) / Self.rms(systemFromSidecar.samples[window]))
      #expect(abs(micDifference) < 0.1, "mic master vs sidecar \(micDifference) dB")
      #expect(abs(systemDifference) < 0.1, "system master vs sidecar \(systemDifference) dB")
      #expect(abs(Self.rms(micFromMaster.samples[window]) - 0.3536) < 0.01)
      #expect(abs(Self.rms(systemFromMaster.samples[window]) - 0.1768) < 0.005)

      await #expect(throws: CodecError.laneNotInAsset(.mixed)) {
        try await codec.decode(withoutSidecars, lane: .mixed)
      }
    }

    @Test func decodesThePhoneM4aForMixedAndRejectsMic() async throws {
      let directory = try Fixtures.temporaryDirectory("codec")
      defer { try? FileManager.default.removeItem(at: directory) }
      let asset = try Self.makePhoneAsset(in: directory)
      let codec = AVFoundationAudioCodec()
      let mixed = try await codec.decode(asset, lane: .mixed)
      #expect(
        abs(mixed.samples.count - 32_000) <= 2_048, "\(mixed.samples.count) samples (AAC priming)")
      let level = 20 * log10(Self.rms(mixed.samples[8_000..<28_000]) / 0.3536)
      #expect(abs(level) < 1, "1 kHz through AAC at \(level) dB")
      await #expect(throws: CodecError.laneNotInAsset(.mic)) {
        try await codec.decode(asset, lane: .mic)
      }
    }

    @Test func mixdownIsAACMonoOfTheRightLength() async throws {
      let directory = try Fixtures.temporaryDirectory("codec")
      defer { try? FileManager.default.removeItem(at: directory) }
      let asset = try Self.makeCallAsset(in: directory)
      let codec = AVFoundationAudioCodec()
      let target = RecordingLayout(asset: asset).mixdown(codec.mixdownFormat)
      try await codec.mixdown(asset, to: target)
      #expect(target.lastPathComponent == "audio.m4a")
      let file = try AVAudioFile(forReading: target)
      #expect(file.fileFormat.channelCount == 1)
      #expect(file.fileFormat.streamDescription.pointee.mFormatID == kAudioFormatMPEG4AAC)
      #expect(abs(Double(file.length) / file.fileFormat.sampleRate - 2) < 0.1)

      // Both lanes are 1 kHz in phase: the mono average is (0.5 + 0.25) / 2.
      let decoded = try AVFoundationAudioCodec.decode(url: target, channel: 0, lane: .mixed)
      let level = Self.rms(decoded.samples[8_000..<28_000])
      #expect(abs(20 * log10(level / (0.375 / Float(2.0.squareRoot())))) < 1)

      // A phone asset is copied, not re-encoded.
      let phone = try Self.makePhoneAsset(in: directory)
      let copy = RecordingLayout(asset: phone).mixdown(.m4aAAC)
      try await codec.mixdown(phone, to: copy)
      #expect(try Data(contentsOf: copy) == Data(contentsOf: phone.url))
    }

    @Test func wavMasterDecodesThroughAVFoundation() async throws {
      let directory = try Fixtures.temporaryDirectory("codec")
      defer { try? FileManager.default.removeItem(at: directory) }
      let url = directory.appendingPathComponent("recording.wav")
      try FileManager.default.copyItem(at: Fixtures.url("audio/sweep-3s.wav"), to: url)
      let asset = AudioAsset(
        id: UUID(), meetingID: UUID(), url: url, format: .wav16kInt16, lanes: [.mixed],
        retention: .keepForever)
      let decoded = try await AVFoundationAudioCodec().decode(asset, lane: .mixed)
      let reference = try WAVAudioDecoder.read(url)
      #expect(decoded.samples.count == reference.samples.count)
      #expect(zip(decoded.samples, reference.samples).allSatisfy { abs($0 - $1) < 1e-4 })
    }
  }
#endif
