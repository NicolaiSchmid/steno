import Foundation
import StenoCore
import Testing

@testable import StenoSpeech

#if canImport(FluidAudio)
  /// What `FluidDiarizer` decides before it touches the framework. The
  /// framework calls themselves run in `ModelIntegrationTests` behind
  /// `STENO_MODEL_TESTS`.
  @Suite struct FluidDiarizerTests {
    /// Under one second there is no embedding window to cluster, which
    /// FluidAudio would report as `noSpeechDetected`; the diarizer answers
    /// with no speakers and never loads (or downloads) the models for it.
    @Test func audioUnderOneSecondGivesNoClustersWithoutLoadingModels() async throws {
      let directory = try Fixtures.temporaryDirectory("diarizer")
      defer { try? FileManager.default.removeItem(at: directory) }
      let downloader = FakeModelDownloader()
      let store = ModelStore(directory: directory, downloader: downloader)
      let diarizer = FluidDiarizer(models: store)
      let short = AudioBuffer16k(samples: [Float](repeating: 0.1, count: 8_000))
      let result = try await diarizer.diarize(short)
      #expect(result.clusters.isEmpty)
      #expect(try await diarizer.diarize(AudioBuffer16k(samples: [])).clusters.isEmpty)
      #expect(await downloader.downloads.count == 0)
      #expect(store.installedAssets().isEmpty)
    }
  }
#endif
