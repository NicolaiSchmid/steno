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
    #expect(seen.map(\.fractionCompleted) == [0.25, 0.75, 1])
    #expect(seen.map(\.phase) == ["a", "b", "installed"])
    #expect(seen.allSatisfy { $0.asset == .offlineDiarizer })
    #expect(store.isInstalled(.offlineDiarizer))
    #expect(store.installedAssets() == [.offlineDiarizer])
    #expect(
      store.directory(for: .offlineDiarizer).path.hasSuffix("fluidaudio/speaker-diarization"))

    // Installed: one synthetic event, no second download.
    var again: [ModelDownloadProgress] = []
    for try await progress in await store.ensure(.offlineDiarizer) { again.append(progress) }
    #expect(
      again == [
        ModelDownloadProgress(asset: .offlineDiarizer, fractionCompleted: 1, phase: "installed")
      ])
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
    #expect(a.last?.phase == "installed")
    #expect(b.last?.phase == "installed")
    #expect(a.map(\.fractionCompleted).contains(0.5))
    #expect(b.map(\.fractionCompleted).contains(0.5), "late subscribers see the latest progress")
    #expect(await downloader.downloads.count == 1)
    #expect(store.isInstalled(.parakeetV3))
  }

  @Test func downloadFailureSurfacesAndLeavesNothingInstalled() async throws {
    struct Boom: Error, Equatable {}
    let downloader = FakeModelDownloader(failure: Boom(), failureCount: 1)
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
    await #expect(throws: ModelDownloadError.self) {
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
    }
    #expect(Set(ModelAsset.allCases.map(\.relativePath)).count == ModelAsset.allCases.count)
    #expect(
      ModelAsset.whisperLargeV3Turbo.whisperVariant == "openai_whisper-large-v3-v20240930_turbo")
    #expect(ModelAsset.parakeetV3.whisperVariant == nil)
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
