import Foundation
import StenoAudio
import StenoCore
import StenoLLM
import Testing

#if canImport(AVFoundation)
  import AVFoundation

  /// `steno dev bakeoff` against the binary with `--fake-engines`: the codec
  /// wiring over files core's WAV reader rejects (48 kHz CAF, 48 kHz WAV,
  /// AAC m4a), all built in setup from `AudioFixtures`. No model is
  /// downloaded and no database is opened.
  @Suite(.serialized) struct BakeoffCLITests {
    /// `tone-2s.caf`, `tone-3s.wav` and `tone-1s.m4a` at 48 kHz, each with
    /// a reference of one "<referenceWord> segment n" per second; the fake
    /// engine says "fake segment n", so `fake` matches it word for word and
    /// any other word puts one substitution per segment into the WER.
    static func makeAudioFolder(in home: URL, referenceWord: String = "fake") throws -> URL {
      let audio = home.appendingPathComponent("audio", isDirectory: true)
      try FileManager.default.createDirectory(at: audio, withIntermediateDirectories: true)
      try AudioFixtures.writeCAF(
        AudioFixtures.tone(frequency: 440, seconds: 2),
        to: audio.appendingPathComponent("tone-2s.caf"))
      try AudioFixtures.writeWAV(
        AudioFixtures.tone(frequency: 440, seconds: 3),
        to: audio.appendingPathComponent("tone-3s.wav"))
      try writeM4A(
        AudioFixtures.tone(frequency: 440, seconds: 1),
        to: audio.appendingPathComponent("tone-1s.m4a"))
      for (name, seconds) in [("tone-2s", 2), ("tone-3s", 3), ("tone-1s", 1)] {
        let reference = (1...seconds).map { "\(referenceWord) segment \($0)" }
          .joined(separator: " ")
        try Data(reference.utf8).write(to: audio.appendingPathComponent("\(name).ref.txt"))
      }
      return audio
    }

    /// AAC mono 48 kHz through `AVAudioFile`, a phone recording's shape.
    static func writeM4A(_ samples: [Float], to url: URL) throws {
      let format = AVAudioFormat(
        standardFormatWithSampleRate: AudioFixtures.sampleRate, channels: 1)!
      let file = try AVAudioFile(
        forWriting: url,
        settings: [
          AVFormatIDKey: kAudioFormatMPEG4AAC, AVSampleRateKey: AudioFixtures.sampleRate,
          AVNumberOfChannelsKey: 1, AVEncoderBitRateKey: 64_000,
        ], commonFormat: .pcmFormatFloat32, interleaved: false)
      let buffer = AVAudioPCMBuffer(
        pcmFormat: format, frameCapacity: AVAudioFrameCount(samples.count))!
      buffer.frameLength = AVAudioFrameCount(samples.count)
      samples.withUnsafeBufferPointer {
        buffer.floatChannelData![0].update(from: $0.baseAddress!, count: samples.count)
      }
      try file.write(from: buffer)
    }

    @Test func decodesCAFWAVAndM4AAtAnySampleRateThroughTheCodec() throws {
      let home = try Fixtures.temporaryDirectory("steno-bakeoff-home")
      defer { try? FileManager.default.removeItem(at: home) }
      let audio = try Self.makeAudioFolder(in: home)
      let out = home.appendingPathComponent("out", isDirectory: true)

      let result = try CLITests.run(
        [
          "dev", "bakeoff", audio.path, "--engines", "parakeet-v3", "whisperkit-large-v3-turbo",
          "--fake-engines", "--out", out.path,
        ], home: home)
      #expect(result.status == 0, "\(result.stderr)")
      // Files sort by name; every duration is exact after the 48 kHz to
      // 16 kHz resample, and the fake reproduces the reference word for word.
      #expect(result.stdout.contains("| tone-1s.m4a | parakeet-v3 | 1.00 |"))
      #expect(result.stdout.contains("| tone-2s.caf | parakeet-v3 | 2.00 |"))
      #expect(result.stdout.contains("| tone-3s.wav | parakeet-v3 | 3.00 |"))
      #expect(result.stdout.contains("| tone-2s.caf | whisperkit-large-v3-turbo | 2.00 |"))
      let rows = result.stdout.split(separator: "\n").filter { $0.hasPrefix("| tone-") }
      #expect(rows.count == 6)
      for row in rows {
        let cells = row.split(separator: "|").map { $0.trimmingCharacters(in: .whitespaces) }
        #expect(cells[5] + ".00" == cells[2], "one fake segment per second: \(row)")
        #expect(cells[6] == "0.0 %", "WER against the exact reference: \(row)")
      }
      #expect(result.stdout.contains("| parakeet-v3 | 3 |"), "the per-engine summary")
      #expect(result.stdout.hasSuffix("reports: \(out.path)\n"))

      let written = try FileManager.default.contentsOfDirectory(atPath: out.path).sorted()
      #expect(written.contains("report.json") && written.contains("report.md"))
      #expect(written.contains("tone-2s.parakeet-v3.json"))
      #expect(written.count == 8, "\(written)")
      #expect(
        !FileManager.default.fileExists(
          atPath: home.appendingPathComponent("Library/Application Support/Steno").path),
        "fake engines open neither the model store nor the database")
    }

    /// Table rows as trimmed cells: file, engine, audio s, wall s, RTFx,
    /// segments, WER, WER (cleaned), cleanup requests, flips, language.
    static func rows(in stdout: String) -> [[String]] {
      stdout.split(separator: "\n").filter { $0.hasPrefix("| tone-") }.map { row in
        row.split(separator: "|").map { $0.trimmingCharacters(in: .whitespaces) }
      }
    }

    /// `--cleanup` without an endpoint in the settings prints one notice and
    /// runs without the cleaned columns; with one, every transcript that
    /// has a reference goes through `LLMTranscriptCleaner` on the stub and
    /// the table shows the WER after it and the requests it took.
    @Test func cleanupRunsOnTheConfiguredEndpointOrSkipsWithANotice() async throws {
      let home = try Fixtures.temporaryDirectory("steno-bakeoff-home")
      defer { try? FileManager.default.removeItem(at: home) }
      let audio = try Self.makeAudioFolder(in: home, referenceWord: "real")
      let db = home.appendingPathComponent("steno.sqlite")
      let arguments = [
        "dev", "bakeoff", audio.path, "--engines", "parakeet-v3", "--fake-engines", "--cleanup",
        "--db", db.path,
      ]

      let skipped = try CLITests.run(
        arguments + ["--out", home.appendingPathComponent("skipped").path], home: home)
      #expect(skipped.status == 0, "\(skipped.stderr)")
      #expect(
        skipped.stderr
          == "cleanup skipped: no LLM endpoint configured (set Settings.llmBaseURL and llmModel)\n")
      let skippedRows = Self.rows(in: skipped.stdout)
      #expect(skippedRows.count == 3)
      for row in skippedRows {
        #expect(row[6] == "33.3 %", "one substitution per segment: \(row)")
        #expect(row[7] == "-" && row[8] == "-", "no cleaned WER, no requests: \(row)")
      }
      #expect(skipped.stdout.contains("| parakeet-v3 | 3 | "))
      #expect(skipped.stdout.contains(" | 33.3 % | - | - | 0 |"), "the summary skips cleanup too")

      let server = try StubChatServer()
      defer { server.stop() }
      server.respond(
        with: Scripts.cleanupEcho { _, text in text.replacingOccurrences(of: "fake", with: "real") }
      )
      let store = try MeetingStore.onDisk(at: db)
      var settings = try await SettingsStore(writer: store.writer).load()
      settings.llmBaseURL = server.baseURL
      settings.llmModel = "stub-model"
      try await SettingsStore(writer: store.writer).save(settings)

      let cleaned = try CLITests.run(
        arguments + ["--out", home.appendingPathComponent("cleaned").path], home: home)
      #expect(cleaned.status == 0, "\(cleaned.stderr)")
      #expect(cleaned.stderr.isEmpty)
      let cleanedRows = Self.rows(in: cleaned.stdout)
      #expect(cleanedRows.count == 3)
      for row in cleanedRows {
        #expect(row[6] == "33.3 %", "the raw WER is unchanged: \(row)")
        #expect(row[7] == "0.0 %", "the cleaner's output matches the reference: \(row)")
        #expect(row[8] == "1", "one chunk, one request per file: \(row)")
      }
      #expect(cleaned.stdout.contains(" | 33.3 % | 0.0 % | 3 | 0 |"), "requests summed per engine")
      #expect(server.requests.count == 3)
      #expect(server.requests.allSatisfy { $0.purpose == "cleanup" })
      #expect(
        server.requests.allSatisfy { $0.authorization == nil },
        "no key is configured, so none is sent")
    }
  }
#endif
