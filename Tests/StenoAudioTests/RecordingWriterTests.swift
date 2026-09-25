import Foundation
import StenoCore
import Testing

@testable import StenoAudio

@Suite struct RecordingWriterTests {
  func sine(frequency: Double, amplitude: Float, count: Int) -> [Float] {
    (0..<count).map { amplitude * Float(sin(2 * Double.pi * frequency * Double($0) / 48_000)) }
  }

  @Test func twoLanesRoundTripSampleAccuratelyWithSidecars() throws {
    let directory = try Fixtures.temporaryDirectory("writer")
    defer { try? FileManager.default.removeItem(at: directory) }
    let meetingID = UUID()
    let layout = RecordingLayout(audioFolder: directory, meetingID: meetingID)
    let writer = try RecordingWriter(layout: layout, lanes: [.mic, .system])
    let seconds = 2
    let mic = sine(frequency: 440, amplitude: 0.5, count: 48_000 * seconds)
    let system = sine(frequency: 1_000, amplitude: 0.25, count: 48_000 * seconds)
    for start in stride(from: 0, to: mic.count, by: 480) {
      try mic.withUnsafeBufferPointer { micBuffer in
        try system.withUnsafeBufferPointer { systemBuffer in
          try writer.write(
            LaneFrames(
              frameCount: 480,
              lanes: [micBuffer.baseAddress! + start, systemBuffer.baseAddress! + start]))
        }
      }
    }
    let files = try writer.finish()
    #expect(files.master == layout.master(.caf48kFloat32))
    #expect(files.sidecars16k == [.mic: layout.sidecar(.mic), .system: layout.sidecar(.system)])
    #expect(files.rawMic == nil)
    #expect(files.duration == 2)

    let master = try CAFFile.read(files.master)
    #expect(master.sampleRate == 48_000)
    #expect(master.channels.count == 2)
    #expect(master.frameCount == 96_000)
    #expect(master.channels[0] == mic, "channel 0 is the mic lane, bit for bit")
    #expect(master.channels[1] == system)

    let micSidecar = try WAVAudioDecoder.read(files.sidecars16k[.mic]!)
    let systemSidecar = try WAVAudioDecoder.read(files.sidecars16k[.system]!)
    #expect(micSidecar.samples.count == 32_000)
    #expect(systemSidecar.samples.count == 32_000)
    func rms(_ samples: ArraySlice<Float>) -> Float {
      Float((samples.reduce(0.0) { $0 + Double($1 * $1) } / Double(samples.count)).squareRoot())
    }
    #expect(abs(20 * log10(rms(micSidecar.samples[2_000...]) / 0.3536)) < 0.1)
    #expect(abs(20 * log10(rms(systemSidecar.samples[2_000...]) / 0.1768)) < 0.1)
    let info = try WAVAudioDecoder.info(files.sidecars16k[.mic]!)
    #expect(info.sampleRate == 16_000 && info.channels == 1 && info.bitsPerSample == 16)
  }

  @Test func rawMicLaneIsWrittenBesideTheMaster() throws {
    let directory = try Fixtures.temporaryDirectory("writer")
    defer { try? FileManager.default.removeItem(at: directory) }
    let layout = RecordingLayout(audioFolder: directory, meetingID: UUID())
    let writer = try RecordingWriter(layout: layout, lanes: [.mic, .system], keepRawMic: true)
    let processed = [Float](repeating: 0.1, count: 480)
    let raw = [Float](repeating: 0.4, count: 480)
    let system = [Float](repeating: 0, count: 480)
    for _ in 0..<10 {
      try processed.withUnsafeBufferPointer { p in
        try system.withUnsafeBufferPointer { s in
          try raw.withUnsafeBufferPointer { r in
            try writer.write(
              LaneFrames(
                frameCount: 480, lanes: [p.baseAddress!, s.baseAddress!], rawMic: r.baseAddress!))
          }
        }
      }
    }
    let files = try writer.finish()
    let rawURL = try #require(files.rawMic)
    #expect(rawURL.lastPathComponent == "mic.raw.caf")
    let rawFile = try CAFFile.read(rawURL)
    #expect(rawFile.channels.count == 1)
    #expect(rawFile.frameCount == 4_800)
    #expect(rawFile.channels[0].allSatisfy { $0 == 0.4 })
    #expect(try CAFFile.read(files.master).channels[0].allSatisfy { $0 == 0.1 })
  }

  @Test func inPersonWritesOneChannelAndTheMixedSidecar() throws {
    let directory = try Fixtures.temporaryDirectory("writer")
    defer { try? FileManager.default.removeItem(at: directory) }
    let layout = RecordingLayout(audioFolder: directory, meetingID: UUID())
    let writer = try RecordingWriter(layout: layout, lanes: [.mixed])
    let frame = sine(frequency: 500, amplitude: 0.5, count: 480)
    for _ in 0..<50 {
      try frame.withUnsafeBufferPointer {
        try writer.write(LaneFrames(frameCount: 480, lanes: [$0.baseAddress!]))
      }
    }
    let files = try writer.finish()
    #expect(files.sidecars16k.keys.sorted { $0.rawValue < $1.rawValue } == [.mixed])
    #expect(files.sidecars16k[.mixed]?.lastPathComponent == "mixed.wav")
    #expect(try CAFFile.read(files.master).channels.count == 1)
    #expect(files.duration == 0.5)
  }

  @Test func wrongFrameShapeAndDoubleFinishThrow() throws {
    let directory = try Fixtures.temporaryDirectory("writer")
    defer { try? FileManager.default.removeItem(at: directory) }
    let writer = try RecordingWriter(directory: directory, lanes: [.mixed])
    let short = [Float](repeating: 0, count: 100)
    #expect(throws: CaptureError.self) {
      try short.withUnsafeBufferPointer {
        try writer.write(LaneFrames(frameCount: 100, lanes: [$0.baseAddress!]))
      }
    }
    _ = try writer.finish()
    #expect(throws: CaptureError.self) { try writer.finish() }
  }

  /// The master is readable before `finish()`: the data chunk says -1 and the
  /// reader takes everything to the end of the file.
  @Test func unfinishedMasterIsReadableToTheLastFrame() throws {
    let directory = try Fixtures.temporaryDirectory("writer")
    defer { try? FileManager.default.removeItem(at: directory) }
    let url = directory.appendingPathComponent("streaming.caf")
    let writer = try CAFStreamWriter(url: url, sampleRate: 48_000, channels: 2)
    let frame = [Float](repeating: 0.25, count: 960)
    try frame.withUnsafeBufferPointer {
      try writer.write(interleaved: $0.baseAddress!, frameCount: 480)
    }
    try frame.withUnsafeBufferPointer {
      try writer.write(interleaved: $0.baseAddress!, frameCount: 480)
    }
    let partial = try CAFFile.read(url)
    #expect(partial.frameCount == 960)
    #expect(partial.channels[1][959] == 0.25)
    try writer.finish()
    let finished = try CAFFile.read(url)
    #expect(finished.frameCount == 960)
    let bytes = try Data(contentsOf: url)
    #expect(bytes.count == 68 + 960 * 8, "8 caff + 44 desc + 16 data header")
    let size = bytes[56..<64].reduce(Int64(0)) { $0 << 8 | Int64($1) }
    #expect(size == 4 + 960 * 8)
  }

  @Test func cafReaderRejectsGarbage() {
    #expect(throws: CAFReadError.self) { try CAFFile.read(Data(repeating: 0x41, count: 64)) }
    #expect(throws: CAFReadError.self) { try CAFFile.read(Data("caff".utf8)) }
  }
}
