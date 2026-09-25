import Foundation
import StenoCore
import Testing

@testable import StenoSpeech

/// The `say` fixtures under `Tests/Fixtures/speech` are committed once and
/// never regenerated (`say` changes with macOS releases), so their hashes
/// live in the folder's own `MANIFEST.sha256` rather than in the generated
/// root manifest, and this suite pins them. `two-speakers` is Anna and
/// Samantha (two female voices the community-1 diarizer hears as one
/// speaker); `two-speakers-mf` is the same dialogue as Anna and Daniel,
/// rendered once on macOS 26.7, which it separates.
@Suite struct SpeechFixtureTests {
  static let names = [
    "de-short", "de-short-2", "en-short", "denglish", "two-speakers", "two-speakers-mf",
  ]

  @Test func everyFixtureIsListedWithItsReferenceAndHash() throws {
    let manifest = try String(
      contentsOf: Fixtures.url("speech/MANIFEST.sha256"), encoding: .utf8)
    var listed: [String: String] = [:]
    for line in manifest.split(separator: "\n") {
      let parts = line.split(separator: " ", omittingEmptySubsequences: true)
      guard parts.count == 2 else { continue }
      listed[String(parts[1])] = String(parts[0])
    }
    for name in Self.names {
      for file in ["\(name).wav", "\(name).ref.txt"] {
        let data = try Fixtures.data("speech/\(file)")
        #expect(listed[file] == ContentHash.sha256Hex(data), "\(file)")
      }
      let reference = try String(
        contentsOf: Fixtures.url("speech/\(name).ref.txt"), encoding: .utf8)
      #expect(WordErrorRate.normalise(reference).count >= 5, "\(name)")
    }
    #expect(listed.count == Self.names.count * 2)
  }

  @Test func everyFixtureIsShort16kMonoInt16() throws {
    for name in Self.names {
      let info = try WAVAudioDecoder.info(Fixtures.url("speech/\(name).wav"))
      #expect(info.sampleRate == 16_000, "\(name)")
      #expect(info.channels == 1, "\(name)")
      #expect(info.bitsPerSample == 16, "\(name)")
      #expect(info.frameCount <= 10 * 16_000, "\(name) is over ten seconds")
      #expect(info.frameCount >= 16_000, "\(name) is under one second")
    }
  }

  @Test(arguments: ["two-speakers", "two-speakers-mf"])
  func twoSpeakersHasFourTurnsSeparatedBySilence(name: String) throws {
    let audio = try WAVAudioDecoder.read(Fixtures.url("speech/\(name).wav"))
    // Energy per 100 ms frame; the four turns are separated by 0.4 s of
    // digital silence, so at least three silent stretches are inside.
    let frame = 1_600
    var silentRuns = 0
    var inSilence = false
    var index = 0
    while index + frame <= audio.samples.count {
      let energy = audio.samples[index..<index + frame].reduce(0) { $0 + abs($1) } / Float(frame)
      if energy < 1e-4 {
        if !inSilence { silentRuns += 1 }
        inSilence = true
      } else {
        inSilence = false
      }
      index += frame
    }
    #expect(silentRuns >= 3)
    let windows = WhisperWindowRanking.topWindows(
      samples: audio.samples, windowSeconds: 2, count: 1)
    #expect(windows.first?.rms ?? 0 > 0.01, "speech is audible")
  }
}
