import Foundation
import StenoCore
import Testing

@testable import StenoSpeech

@Suite struct ModelStoreTests {
  private func makeStore(_ downloader: FakeModelDownloader) throws -> (ModelStore, URL) {
    let directory = try Fixtures.temporaryDirectory("models")
    return (ModelStore(directory: directory, downloader: downloader), directory)
  }

  @Test func ensureDownloadsOnceAndReportsProgress() async throws {
    let downloader = FakeModelDownloader(steps: [(0.25, "a"), (0.75, "b")])
    let (store, directory) = try makeStore(downloader)
    defer { try? FileManager.default.removeItem(at: directory) }
    #expect(store.isInstalled(.offlineDiarizer) == false)

    var seen: [ModelDownloadProgress] = []
    for try await progress in await store.ensure(.offlineDiarizer) { seen.append(progress) }
    // The downloader's own events, then the end of the stream is the install.
    #expect(seen.map(\.fractionCompleted) == [0.25, 0.75])
    #expect(seen.map(\.phase) == ["a", "b"])
    #expect(store.isInstalled(.offlineDiarizer))
    #expect(store.installedAssets() == [.offlineDiarizer])
    #expect(
      store.directory(for: .offlineDiarizer).path.hasSuffix("fluidaudio/speaker-diarization"))

    // Installed: an already finished stream, no event, no second download.
    var again: [ModelDownloadProgress] = []
    for try await progress in await store.ensure(.offlineDiarizer) { again.append(progress) }
    #expect(again.isEmpty)
    #expect(await downloader.downloads.count == 1)
  }

  @Test func concurrentEnsuresShareOneDownload() async throws {
    let gate = Gate()
    let downloader = FakeModelDownloader(steps: [(0.5, "half")], hold: { await gate.wait() })
    let (store, directory) = try makeStore(downloader)
    defer { try? FileManager.default.removeItem(at: directory) }

    let first = await store.ensure(.parakeetV3)
    let second = await store.ensure(.parakeetV3)
    async let firstEvents = collect(first)
    async let secondEvents = collect(second)
    await gate.open()
    let (a, b) = try await (firstEvents, secondEvents)
    #expect(a.last?.phase == "half", "both streams ended after the one download")
    #expect(b.last?.phase == "half")
    #expect(a.map(\.fractionCompleted).contains(0.5))
    #expect(b.map(\.fractionCompleted).contains(0.5), "late subscribers see the latest progress")
    #expect(await downloader.downloads.count == 1)
    #expect(store.isInstalled(.parakeetV3))
  }

  @Test func downloadFailureSurfacesAndLeavesNothingInstalled() async throws {
    struct Boom: Error, Equatable {}
    let downloader = FakeModelDownloader(failure: Boom())
    let (store, directory) = try makeStore(downloader)
    defer { try? FileManager.default.removeItem(at: directory) }

    await #expect(throws: Boom.self) {
      try await store.ensureInstalled(.parakeetUltra)
    }
    #expect(store.isInstalled(.parakeetUltra) == false)
    // A failed job is cleared, so the next call downloads again.
    try await store.ensureInstalled(.parakeetUltra)
    #expect(await downloader.downloads.count == 2)
  }

  @Test func incompleteDownloadIsAnError() async throws {
    let downloader = FakeModelDownloader(writesMarkers: false)
    let (store, directory) = try makeStore(downloader)
    defer { try? FileManager.default.removeItem(at: directory) }
    await #expect(throws: StenoSpeechError.self) {
      try await store.ensureInstalled(.whisperLargeV3Turbo)
    }
  }

  @Test func removeDeletesTheAssetDirectory() async throws {
    let (store, directory) = try makeStore(FakeModelDownloader())
    defer { try? FileManager.default.removeItem(at: directory) }
    try await store.ensureInstalled(.parakeetDE)
    #expect(store.isInstalled(.parakeetDE))
    #expect(store.installedSize(of: .parakeetDE) == 0, "markers are empty files")
    try await store.remove(.parakeetDE)
    #expect(store.isInstalled(.parakeetDE) == false)
    #expect(!FileManager.default.fileExists(atPath: store.directory(for: .parakeetDE).path))
    try await store.remove(.parakeetDE)
  }

  @Test func removingAnAssetMidDownloadIsRefused() async throws {
    let gate = Gate()
    let downloader = FakeModelDownloader(hold: { await gate.wait() })
    let (store, directory) = try makeStore(downloader)
    defer { try? FileManager.default.removeItem(at: directory) }
    let stream = await store.ensure(.offlineDiarizer)
    async let events = collect(stream)
    for _ in 0..<50 { await Task.yield() }
    await #expect(throws: StenoSpeechError.downloadInProgress(.offlineDiarizer)) {
      try await store.remove(.offlineDiarizer)
    }
    await gate.open()
    _ = try await events
    #expect(store.isInstalled(.offlineDiarizer))
    try await store.remove(.offlineDiarizer)
    #expect(store.isInstalled(.offlineDiarizer) == false)
  }

  /// A download killed between bundles (the process died, the disk filled
  /// up) leaves some of the required files behind. That directory is not an
  /// install, and the next `ensure` downloads again and completes it.
  @Test func aKilledDownloadIsNotInstalledAndIsCompletedByTheNextEnsure() async throws {
    let downloader = FakeModelDownloader()
    let (store, directory) = try makeStore(downloader)
    defer { try? FileManager.default.removeItem(at: directory) }
    let target = store.directory(for: .parakeetV3)
    let partial = Array(ModelAsset.parakeetV3.requiredFiles.dropLast())
    for name in partial {
      try FakeModelDownloader.writeMarker(at: target.appendingPathComponent(name))
    }
    try Data("junk".utf8).write(to: target.appendingPathComponent("Encoder.mlmodelc.download"))
    #expect(store.isInstalled(.parakeetV3) == false)
    #expect(store.installedSize(of: .parakeetV3) == nil)
    #expect(store.installedAssets().isEmpty)

    try await store.ensureInstalled(.parakeetV3)
    #expect(store.isInstalled(.parakeetV3))
    #expect(await downloader.downloads.entries == [.parakeetV3])
    let files = try FileManager.default.contentsOfDirectory(atPath: target.path).sorted()
    #expect(files == (ModelAsset.parakeetV3.requiredFiles + ["Encoder.mlmodelc.download"]).sorted())
    // Size counts every regular file under the asset, whatever wrote it; the
    // fake rewrote the required files as empty markers, the leftover stays.
    #expect(store.installedSize(of: .parakeetV3) == Int64("junk".utf8.count))
  }

  /// Both frameworks write each file of a compiled bundle straight into its
  /// final directory, so a kill inside a bundle leaves the `.mlmodelc`
  /// directory present without its root `coremldata.bin`. FluidAudio's own
  /// cache check refuses that shape; so does the store, and `ensure` repairs it.
  @Test func aBundleWithoutCoremldataIsNotInstalledUntilEnsureRepairsIt() async throws {
    let downloader = FakeModelDownloader()
    let (store, directory) = try makeStore(downloader)
    defer { try? FileManager.default.removeItem(at: directory) }
    try await store.ensureInstalled(.offlineDiarizer)
    let bundle = store.directory(for: .offlineDiarizer).appendingPathComponent("Embedding.mlmodelc")
    try FileManager.default.removeItem(at: bundle.appendingPathComponent("coremldata.bin"))
    try Data("weights".utf8).write(to: bundle.appendingPathComponent("weights.bin"))
    #expect(store.isInstalled(.offlineDiarizer) == false, "a bundle directory alone is not enough")
    #expect(store.installedSize(of: .offlineDiarizer) == nil)

    try await store.ensureInstalled(.offlineDiarizer)
    #expect(await downloader.downloads.count == 2, "the second ensure downloaded again")
    #expect(store.isInstalled(.offlineDiarizer))
  }

  /// FluidAudio stages every file as `<name>.partial` and renames it when it
  /// is complete; WhisperKit's Hub client uses `.incomplete`. A bundle that
  /// still holds one is mid-download, whatever else is in it.
  @Test func aBundleWithAStagingFileIsNotInstalled() async throws {
    let (store, directory) = try makeStore(FakeModelDownloader())
    defer { try? FileManager.default.removeItem(at: directory) }
    try await store.ensureInstalled(.parakeetUltra)
    let bundle = store.directory(for: .parakeetUltra).appendingPathComponent("Encoder.mlmodelc")
    let weights = bundle.appendingPathComponent("weights", isDirectory: true)
    try FileManager.default.createDirectory(at: weights, withIntermediateDirectories: true)
    try Data().write(to: weights.appendingPathComponent("weight.bin.partial"))
    #expect(store.isInstalled(.parakeetUltra) == false)
    try FileManager.default.removeItem(at: weights.appendingPathComponent("weight.bin.partial"))
    try Data().write(to: weights.appendingPathComponent("weight.bin.abc123.incomplete"))
    #expect(store.isInstalled(.parakeetUltra) == false)
    try FileManager.default.removeItem(
      at: weights.appendingPathComponent("weight.bin.abc123.incomplete"))
    #expect(store.isInstalled(.parakeetUltra))
  }

  /// WhisperKit fetches its tokenizer after the weights, into the Hub cache
  /// beside them rather than into the model folder. Without it the first
  /// load goes online, so the weights alone are not an install.
  @Test func whisperNeedsItsTokenizerBesideTheWeights() async throws {
    let downloader = FakeModelDownloader()
    let (store, directory) = try makeStore(downloader)
    defer { try? FileManager.default.removeItem(at: directory) }
    let target = store.directory(for: .whisperLargeV3Turbo)
    for name in ModelAsset.whisperLargeV3Turbo.requiredFiles {
      try FakeModelDownloader.writeMarker(at: target.appendingPathComponent(name))
    }
    #expect(store.isInstalled(.whisperLargeV3Turbo) == false, "weights without the tokenizer")

    try await store.ensureInstalled(.whisperLargeV3Turbo)
    #expect(await downloader.downloads.entries == [.whisperLargeV3Turbo])
    #expect(store.isInstalled(.whisperLargeV3Turbo))
    let tokenizer = store.frameworkRoot(for: .whisperLargeV3Turbo)
      .appendingPathComponent("models/openai/whisper-large-v3/tokenizer.json")
    #expect(FileManager.default.fileExists(atPath: tokenizer.path))
    #expect(
      store.frameworkRoot(for: .whisperLargeV3Turbo).path.hasSuffix("/whisperkit"),
      "the framework root is WhisperKit's downloadBase")
    // Removing the asset removes the model folder; the tokenizer is Hub
    // cache shared with any other Whisper variant and stays.
    try await store.remove(.whisperLargeV3Turbo)
    #expect(store.isInstalled(.whisperLargeV3Turbo) == false)
    #expect(FileManager.default.fileExists(atPath: tokenizer.path))
  }

  @Test func everyStreamOpenedDuringADownloadEndsWithItsOutcome() async throws {
    struct Boom: Error, Equatable {}
    let (store, directory) = try makeStore(FakeModelDownloader(failure: Boom()))
    defer { try? FileManager.default.removeItem(at: directory) }
    let first = await store.ensure(.parakeetUltra)
    let second = await store.ensure(.parakeetUltra)
    await #expect(throws: Boom.self) { _ = try await collect(first) }
    // The job is gone by now; the other stream, read late, still throws.
    await #expect(throws: Boom.self) { _ = try await collect(second) }
    #expect(store.isInstalled(.parakeetUltra) == false)
    try await store.ensureInstalled(.parakeetUltra)
  }

  @Test func differentAssetsDownloadConcurrentlyAndIndependently() async throws {
    let gate = Gate()
    let downloader = FakeModelDownloader(hold: { await gate.wait() })
    let (store, directory) = try makeStore(downloader)
    defer { try? FileManager.default.removeItem(at: directory) }
    let diarizer = await store.ensure(.offlineDiarizer)
    let whisper = await store.ensure(.whisperLargeV3Turbo)
    async let diarizerEvents = collect(diarizer)
    async let whisperEvents = collect(whisper)
    // Both downloads are started (and held) before either finishes.
    for _ in 0..<50 { await Task.yield() }
    #expect(await Set(downloader.downloads.entries) == [.offlineDiarizer, .whisperLargeV3Turbo])
    #expect(store.installedAssets().isEmpty)
    await gate.open()
    let (a, b) = try await (diarizerEvents, whisperEvents)
    #expect(a.map(\.phase) == ["downloading", "done"])
    #expect(b.map(\.phase) == ["downloading", "done"])
    #expect(Set(store.installedAssets()) == [.offlineDiarizer, .whisperLargeV3Turbo])
  }

  @Test func defaultDirectoryFollowsHome() {
    #expect(ModelStore.defaultDirectory().path.hasSuffix("Steno/Models"))
    let store = ModelStore(downloader: FakeModelDownloader())
    #expect(store.directory == ModelStore.defaultDirectory().standardizedFileURL)
  }

  @Test func assetTableIsConsistent() {
    for asset in ModelAsset.allCases {
      #expect(!asset.requiredFiles.isEmpty, "\(asset)")
      #expect(asset.approximateBytes > 0, "\(asset)")
      #expect(!asset.relativePath.hasPrefix("/"), "\(asset)")
      #expect(asset.relativePath.hasPrefix(asset.frameworkRoot + "/"), "\(asset)")
      #expect(asset.relativePath.hasSuffix("/" + asset.modelFolder), "\(asset)")
      #expect(
        asset.requiredPaths.allSatisfy { $0.hasPrefix(asset.frameworkRoot + "/") },
        "\(asset): every required path lives under the framework root")
      for file in asset.requiredFiles {
        #expect(asset.requiredPaths.contains("\(asset.relativePath)/\(file)"), "\(asset)")
      }
    }
    #expect(Set(ModelAsset.allCases.map(\.relativePath)).count == ModelAsset.allCases.count)
    // The German fine-tune keeps the v3 folder name under its own root.
    #expect(ModelAsset.parakeetDE.modelFolder == ModelAsset.parakeetV3.modelFolder)
    #expect(ModelAsset.parakeetDE.frameworkRoot != ModelAsset.parakeetV3.frameworkRoot)
    // WhisperKit's variant is the asset directory's name; the tokenizer is
    // the one path outside the asset directory.
    let store = ModelStore(downloader: FakeModelDownloader())
    #expect(
      store.directory(for: .whisperLargeV3Turbo).lastPathComponent
        == ModelAsset.whisperLargeV3Turbo.modelFolder)
    #expect(
      ModelAsset.whisperLargeV3Turbo.requiredPaths
        == ModelAsset.whisperLargeV3Turbo.requiredFiles.map {
          "whisperkit/models/argmaxinc/whisperkit-coreml/openai_whisper-large-v3-v20240930_turbo/\($0)"
        } + ["whisperkit/models/openai/whisper-large-v3/tokenizer.json"])
    #expect(
      store.frameworkRoot(for: .offlineDiarizer).appendingPathComponent("speaker-diarization").path
        == store.directory(for: .offlineDiarizer).path)
  }

  private func collect(_ stream: AsyncThrowingStream<ModelDownloadProgress, any Error>)
    async throws -> [ModelDownloadProgress]
  {
    var events: [ModelDownloadProgress] = []
    for try await event in stream { events.append(event) }
    return events
  }
}

/// Holds a fake download open until the test opens the gate.
actor Gate {
  private var opened = false
  private var waiters: [CheckedContinuation<Void, Never>] = []

  func wait() async {
    if opened { return }
    await withCheckedContinuation { waiters.append($0) }
  }

  func open() {
    opened = true
    for waiter in waiters { waiter.resume() }
    waiters = []
  }
}
