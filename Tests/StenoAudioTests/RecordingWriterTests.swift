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
    let writer = try RecordingWriter(layout: RecordingLayout(directory: directory), lanes: [.mixed])
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
    #expect(bytes.count == CAFStreamWriter.headerSize + 960 * 8, "the header, then the samples")
    let size = bytes[56..<64].reduce(Int64(0)) { $0 << 8 | Int64($1) }
    #expect(size == 4 + 960 * 8)
  }

  /// A process killed inside a write leaves a partial trailing frame; the
  /// reader takes whole frames and ignores the tail.
  @Test func aTruncatedMasterReadsWholeFramesOnly() throws {
    let directory = try Fixtures.temporaryDirectory("writer")
    defer { try? FileManager.default.removeItem(at: directory) }
    let url = directory.appendingPathComponent("killed.caf")
    let writer = try CAFStreamWriter(url: url, sampleRate: 48_000, channels: 2)
    let frame = (0..<960).map { Float($0) }
    try frame.withUnsafeBufferPointer {
      try writer.write(interleaved: $0.baseAddress!, frameCount: 480)
    }
    let handle = try FileHandle(forWritingTo: url)
    try handle.truncate(atOffset: 68 + 479 * 8 + 5)
    try handle.close()
    let partial = try CAFFile.read(url)
    #expect(partial.frameCount == 479)
    #expect(partial.channels[0].last == 956)
    #expect(partial.channels[1].last == 957)
  }

  /// The sidecar is the master's lane at a third of the rate: an onset at
  /// 1.0 s on the mic lane lands at 16 000 samples in `mic.wav` plus the
  /// resampler's 32-sample group delay, exact zeros before it, and nothing at
  /// all in `system.wav`.
  @Test func sidecarsAlignWithTheMasterAndTheirLane() throws {
    let directory = try Fixtures.temporaryDirectory("writer")
    defer { try? FileManager.default.removeItem(at: directory) }
    let layout = RecordingLayout(audioFolder: directory, meetingID: UUID())
    let writer = try RecordingWriter(layout: layout, lanes: [.mic, .system])
    var mic = [Float](repeating: 0, count: 96_000)
    let tone = sine(frequency: 1_000, amplitude: 0.5, count: 48_000)
    mic.replaceSubrange(48_000..<96_000, with: tone)
    let system = [Float](repeating: 0, count: 96_000)
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

    let master = try CAFFile.read(files.master)
    #expect(master.channels[0][..<48_000].allSatisfy { $0 == 0 })
    #expect(master.channels[0][48_001] != 0)
    #expect(master.channels[1].allSatisfy { $0 == 0 })

    let micSidecar = try WAVAudioDecoder.read(files.sidecars16k[.mic]!)
    let systemSidecar = try WAVAudioDecoder.read(files.sidecars16k[.system]!)
    #expect(micSidecar.samples.count == 32_000)
    #expect(
      micSidecar.samples[..<16_000].allSatisfy { $0 == 0 }, "causal: nothing before the onset")
    // The low-pass's step response ramps in around the delayed onset, so the
    // first sample above 0.1 lands a few samples either side of 16 032; a
    // frame of misalignment would be 160 samples away.
    let onset = micSidecar.samples.firstIndex { abs($0) > 0.1 }
    #expect(onset.map { (16_020...16_050).contains($0) } == true, "onset at \(onset ?? -1)")
    #expect(systemSidecar.samples.allSatisfy { $0 == 0 }, "the silent lane's sidecar stays silent")
  }

  /// A sidecar whose writer never reached `finish()` (the process died) keeps
  /// its zero-size header with the samples after it, so the RIFF parsers
  /// reject it as malformed (the trailing bytes read as a chunk that runs
  /// past the file). `AVFoundationAudioCodec.decode` relies on exactly that
  /// (`try?`) to rebuild the lane from the master; finishing repairs it.
  @Test func anUnfinishedSidecarIsRejectedUntilFinished() throws {
    let directory = try Fixtures.temporaryDirectory("writer")
    defer { try? FileManager.default.removeItem(at: directory) }
    let url = directory.appendingPathComponent("mic.wav")
    let writer = try WAVStreamWriter(url: url)
    let samples = [Int16](repeating: 1_000, count: 160)
    try samples.withUnsafeBufferPointer { try writer.write($0.baseAddress!, count: 160) }
    #expect(
      try Data(contentsOf: url).count == WAVStreamWriter.headerSize + 320,
      "the samples are on disk")
    #expect(throws: WAVDecodeError.self) { try WAVAudioDecoder.read(url) }
    #expect(throws: WAVDecodeError.self) { try WAVFile.read(url) }
    try writer.finish()
    #expect(try WAVAudioDecoder.read(url).samples.count == 160)
  }

  @Test func cafReaderRejectsGarbage() {
    #expect(throws: CAFReadError.self) { try CAFFile.read(Data(repeating: 0x41, count: 64)) }
    #expect(throws: CAFReadError.self) { try CAFFile.read(Data("caff".utf8)) }
  }
}
