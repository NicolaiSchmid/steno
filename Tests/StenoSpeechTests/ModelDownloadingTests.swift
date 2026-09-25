import Foundation
import StenoCore
import Synchronization
import Testing

@testable import StenoSpeech

/// A dictionary standing in for FluidAudio's process-wide
/// `ModelRegistry.repoOverrides`.
private final class OverrideTable: Sendable {
  private let table = Mutex<[String: String]>([:])

  var redirect: ScopedRedirect {
    ScopedRedirect(
      read: { self.table.withLock { $0 } },
      write: { value in self.table.withLock { $0 = value } })
  }

  var current: [String: String] { table.withLock { $0 } }
}

/// `LiveModelDownloader` runs every download through these two; the
/// framework calls themselves need a Mac and a network.
@Suite struct DownloadSerializerTests {
  @Test func jobsRunOneAtATimeInSubmissionOrder() async throws {
    let serializer = DownloadSerializer()
    let gate = Gate()
    let log = CallLog<String>()
    async let first: Void = serializer.run {
      await log.record("first start")
      await gate.wait()
      await log.record("first end")
    }
    async let second: Void = serializer.run {
      await log.record("second start")
      await log.record("second end")
    }
    // The second job must not start while the first is held open.
    for _ in 0..<50 { await Task.yield() }
    #expect(await log.entries == ["first start"])
    await gate.open()
    _ = try await (first, second)
    #expect(await log.entries == ["first start", "first end", "second start", "second end"])
  }

  @Test func aFailingJobDoesNotBlockTheNextAndReturnsItsError() async throws {
    struct Boom: Error, Equatable {}
    let serializer = DownloadSerializer()
    await #expect(throws: Boom.self) {
      try await serializer.run { throw Boom() }
    }
    let value = try await serializer.run { 42 }
    #expect(value == 42)
  }

  @Test func redirectIsVisibleOnlyInsideTheJobAndRestoredAfterAThrow() async throws {
    struct Boom: Error {}
    let table = OverrideTable()
    table.redirect.write(["other": "kept"])
    let seen = try await table.redirect.run("v3", to: "de-repo") { table.current }
    #expect(seen == ["other": "kept", "v3": "de-repo"])
    #expect(table.current == ["other": "kept"], "restored on return")

    await #expect(throws: Boom.self) {
      try await table.redirect.run("v3", to: "de-repo") { throw Boom() }
    }
    #expect(table.current == ["other": "kept"], "restored on throw")

    // A pre-existing entry for the same key comes back too.
    table.redirect.write(["v3": "official"])
    _ = try await table.redirect.run("v3", to: "de-repo") { 0 }
    #expect(table.current == ["v3": "official"])
  }

  /// Only the redirected key is put back: an entry someone else writes
  /// while the job runs is not wiped by restoring a snapshot.
  @Test func restoringTheRedirectLeavesOtherEntriesWrittenMeanwhileAlone() async throws {
    let table = OverrideTable()
    _ = try await table.redirect.run("v3", to: "de-repo") {
      var current = table.current
      current["ultra"] = "mirror"
      table.redirect.write(current)
    }
    #expect(table.current == ["ultra": "mirror"])
  }

  #if !canImport(FluidAudio)
    /// The chain is one per process, not one per store: a download of a
    /// second `LiveModelDownloader` waits behind a job of the first. Only
    /// observable where the download itself needs no network (the Linux
    /// stub fails with `unsupportedPlatform` once its turn comes).
    @Test func liveDownloadersShareOneProcessWideChain() async throws {
      let gate = Gate()
      let log = CallLog<String>()
      async let held: Void = LiveModelDownloader.serializer.run {
        await log.record("held start")
        await gate.wait()
        await log.record("held end")
      }
      let root = try Fixtures.temporaryDirectory("live")
      defer { try? FileManager.default.removeItem(at: root) }
      async let second: Void = {
        do {
          try await LiveModelDownloader().download(.parakeetV3, under: root) { _, _ in }
        } catch {
          await log.record("second failed: \(error is ModelDownloadError)")
        }
      }()
      for _ in 0..<50 { await Task.yield() }
      #expect(await log.entries == ["held start"], "the second downloader waits its turn")
      await gate.open()
      _ = try await (held, second)
      #expect(await log.entries == ["held start", "held end", "second failed: true"])
    }
  #endif

  @Test func aConcurrentDownloadNeverSeesAnotherJobsRedirect() async throws {
    // The parakeet-de download redirects the v3 repository; a v3 download
    // requested while it runs must observe the untouched table.
    let serializer = DownloadSerializer()
    let table = OverrideTable()
    let gate = Gate()
    let observed = CallLog<[String: String]>()
    async let german: Void = serializer.run {
      try await table.redirect.run("v3", to: "de-repo") {
        await observed.record(table.current)
        await gate.wait()
      }
    }
    async let official: Void = serializer.run {
      await observed.record(table.current)
    }
    for _ in 0..<50 { await Task.yield() }
    await gate.open()
    _ = try await (german, official)
    #expect(await observed.entries == [["v3": "de-repo"], [:]])
    #expect(table.current.isEmpty)
  }
}
