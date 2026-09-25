import Foundation
import Testing

@testable import StenoCore

@Suite struct WAVAudioDecoderTests {
  @Test func sweepFixtureDecodesTo48000Samples() throws {
    let buffer = try WAVAudioDecoder.read(Fixtures.url("audio/sweep-3s.wav"))
    #expect(buffer.samples.count == 48_000)
    #expect(buffer.duration == 3)
    #expect(buffer.samples.contains { $0 > 0.4 })
    let ceiling: Float = 0.5 + 1.0 / 32768
    #expect(buffer.samples.allSatisfy { abs($0) <= ceiling })
    let info = try WAVAudioDecoder.info(Fixtures.url("audio/sweep-3s.wav"))
    #expect(
      info
        == .init(
          sampleRate: 16_000, channels: 1, bitsPerSample: 16, isFloat: false, frameCount: 48_000))
  }

  @Test func rejects48kHzAndStereo() throws {
    let samples = [Int16](repeating: 0, count: 480)
    #expect(throws: WAVDecodeError.unsupportedFormat("48000 Hz, 1 channel(s); need 16000 Hz mono"))
    {
      try WAVAudioDecoder.read(WAVWriter.data(samples, sampleRate: 48_000))
    }
    #expect(throws: WAVDecodeError.unsupportedFormat("16000 Hz, 2 channel(s); need 16000 Hz mono"))
    {
      try WAVAudioDecoder.read(WAVWriter.data(samples, channels: 2))
    }
  }

  @Test func rejectsGarbage() {
    #expect(throws: WAVDecodeError.malformed("shorter than a RIFF header")) {
      try WAVAudioDecoder.read(Data([1, 2, 3]))
    }
    #expect(throws: WAVDecodeError.malformed("missing RIFF/WAVE tags")) {
      try WAVAudioDecoder.read(Data(repeating: 0x41, count: 64))
    }
    var truncated = WAVWriter.data([Int16](repeating: 0, count: 100))
    truncated.removeLast(10)
    #expect(throws: WAVDecodeError.malformed("chunk data runs past the end of the file")) {
      try WAVAudioDecoder.read(truncated)
    }
  }

  @Test func readsFloat32AndInt16RoundTrips() throws {
    let floats: [Float] = [0, 0.5, -0.5, 1, -1, 0.25]
    let float32 = try WAVAudioDecoder.read(WAVWriter.float32Data(floats))
    #expect(float32.samples == floats)

    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let url = directory.appendingPathComponent("tone.wav")
    try WAVWriter.write(AudioBuffer16k(samples: floats), to: url)
    let int16 = try WAVAudioDecoder.read(url)
    #expect(int16.samples.count == floats.count)
    let tolerance: Float = 1.0 / 32767 + 1.0 / 32768
    for (decoded, original) in zip(int16.samples, floats) {
      let difference: Float = abs(decoded - original)
      #expect(difference <= tolerance)
    }
    #expect(WAVWriter.int16([2, -2]) == [32767, -32767])
  }

  @Test func decoderPrefersTheLaneSidecarAndMixdownCopies() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let master = directory.appendingPathComponent("master.wav")
    let sidecar = directory.appendingPathComponent("mic.wav")
    try WAVWriter.write([Int16](repeating: 1000, count: 16_000), to: master)
    try WAVWriter.write([Int16](repeating: 2000, count: 8_000), to: sidecar)
    let asset = AudioAsset(
      id: SampleData.uuid(70), meetingID: SampleData.meetingID, url: master, format: .wav16kInt16,
      lanes: [.mic, .system], sidecars16k: [.mic: sidecar], retention: .keepForever)
    let decoder = WAVAudioDecoder()
    #expect(try await decoder.decode(asset, lane: .mic).samples.count == 8_000)
    #expect(try await decoder.decode(asset, lane: .system).samples.count == 16_000)

    let mixdown = directory.appendingPathComponent("out/audio.m4a")
    try await decoder.mixdown(asset, to: mixdown)
    #expect(try Data(contentsOf: mixdown) == Data(contentsOf: master))
    try await decoder.mixdown(asset, to: mixdown)
    #expect(FileManager.default.fileExists(atPath: mixdown.path))
  }

  @Test func bufferSlicesClampToItsBounds() {
    let buffer = AudioBuffer16k(samples: (0..<32_000).map { Float($0) })
    #expect(buffer.slice(0.5...1.0).samples.count == 8_000)
    #expect(buffer.slice(0.5...1.0).samples.first == 8_000)
    #expect(buffer.slice(1.5...9.0).samples.count == 8_000)
    #expect(buffer.slice(5...6).samples.isEmpty)
  }

  /// The RIFF and `data` sizes count interleaved samples, so a stereo file
  /// declares exactly the bytes it holds.
  @Test func headerSizesFollowTheInterleavedSampleCount() throws {
    func uint32(_ data: Data, _ offset: Int) -> UInt32 {
      data.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: offset, as: UInt32.self) }
        .littleEndian
    }
    func uint16(_ data: Data, _ offset: Int) -> UInt16 {
      data.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: offset, as: UInt16.self) }
        .littleEndian
    }
    let stereo = WAVWriter.data([Int16](repeating: 0, count: 480), sampleRate: 48_000, channels: 2)
    #expect(stereo.count == 44 + 960)
    #expect(uint32(stereo, 4) == UInt32(stereo.count - 8))
    #expect(uint16(stereo, 20) == 1, "PCM")
    #expect(uint16(stereo, 22) == 2, "channels")
    #expect(uint32(stereo, 24) == 48_000)
    #expect(uint32(stereo, 28) == 48_000 * 4, "byte rate")
    #expect(uint16(stereo, 32) == 4, "block align")
    #expect(uint16(stereo, 34) == 16, "bits per sample")
    #expect(uint32(stereo, 40) == 960, "data bytes")

    let float = WAVWriter.float32Data([Float](repeating: 0.5, count: 10))
    #expect(float.count == 44 + 40)
    #expect(uint32(float, 4) == UInt32(float.count - 8))
    #expect(uint16(float, 20) == 3, "IEEE float")
    #expect(uint16(float, 22) == 1)
    #expect(uint32(float, 28) == 16_000 * 4)
    #expect(uint16(float, 34) == 32)
    #expect(uint32(float, 40) == 40)

    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let url = directory.appendingPathComponent("mono.wav")
    try WAVWriter.write([Int16](repeating: 7, count: 100), to: url)
    #expect(
      try WAVAudioDecoder.info(url)
        == .init(
          sampleRate: 16_000, channels: 1, bitsPerSample: 16, isFloat: false, frameCount: 100)
    )
  }
}
