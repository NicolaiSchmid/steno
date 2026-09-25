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
  /// Submission order is fixed by `enqueue` returning, and "the first job
  /// is running" by a gate the job itself opens, so nothing here waits on
  /// the scheduler.
  @Test func jobsRunOneAtATimeInSubmissionOrder() async throws {
    let serializer = DownloadSerializer()
    let firstStarted = Gate()
    let release = Gate()
    let log = CallLog<String>()
    let first = await serializer.enqueue {
      await log.record("first start")
      await firstStarted.open()
      await release.wait()
      await log.record("first end")
    }
    let second = await serializer.enqueue {
      await log.record("second start")
      await log.record("second end")
    }
    await firstStarted.wait()
    // The second job is queued behind a first that is held open.
    #expect(await log.entries == ["first start"])
    await release.open()
    _ = try await (first.value, second.value)
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
      let heldStarted = Gate()
      let release = Gate()
      let log = CallLog<String>()
      let held = await LiveModelDownloader.serializer.enqueue {
        await log.record("held start")
        await heldStarted.open()
        await release.wait()
        await log.record("held end")
      }
      await heldStarted.wait()
      let root = try Fixtures.temporaryDirectory("live")
      defer { try? FileManager.default.removeItem(at: root) }
      // Submitted while the held job runs, so it lands behind it in the
      // chain and its failure can only be recorded after "held end".
      let second = Task {
        do {
          try await LiveModelDownloader().download(.parakeetV3, under: root) { _, _ in }
        } catch {
          await log.record("second failed: \(error is StenoSpeechError)")
        }
      }
      await release.open()
      _ = try await held.value
      await second.value
      #expect(await log.entries == ["held start", "held end", "second failed: true"])
    }
  #endif

  @Test func aConcurrentDownloadNeverSeesAnotherJobsRedirect() async throws {
    // The parakeet-de download redirects the v3 repository; a v3 download
    // requested while it runs must observe the untouched table.
    let serializer = DownloadSerializer()
    let table = OverrideTable()
    let redirected = Gate()
    let release = Gate()
    let observed = CallLog<[String: String]>()
    let german = await serializer.enqueue {
      try await table.redirect.run("v3", to: "de-repo") {
        await observed.record(table.current)
        await redirected.open()
        await release.wait()
      }
    }
    // Requested while the redirect is in place.
    await redirected.wait()
    let official = await serializer.enqueue {
      await observed.record(table.current)
    }
    await release.open()
    _ = try await (german.value, official.value)
    #expect(await observed.entries == [["v3": "de-repo"], [:]])
    #expect(table.current.isEmpty)
  }
}
