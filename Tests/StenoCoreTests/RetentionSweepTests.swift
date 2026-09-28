import Foundation
import Testing

@testable import StenoCore

@Suite struct RetentionSweepTests {
  @Test func removesMasterSidecarsMixdownAndConfirmedClipsButNeverUnconfirmedClipsOrStrangers()
    async throws
  {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try MeetingStore.inMemory()
    try await store.save(SampleData.meeting())
    for person in SampleData.persons() { try await store.save(person) }
    func file(_ name: String) throws -> URL {
      let url = directory.appendingPathComponent(name)
      try FileManager.default.createDirectory(
        at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
      try Data([1, 2, 3]).write(to: url)
      return url
    }
    let master = try file("master.caf")
    let mic = try file("mic.wav")
    let system = try file("system.wav")
    let mixdown = try file("audio.m4a")
    let namedClip = try file("speakers/named.wav")
    let unnamedClip = try file("speakers/unnamed.wav")
    let stranger = try file("notes.txt")
    let expired = AudioAsset(
      id: SampleData.uuid(70), meetingID: SampleData.meetingID, url: master, format: .caf48kFloat32,
      lanes: [.mic, .system], sidecars16k: [.mic: mic, .system: system], mixdownURL: mixdown,
      retention: .keepDays(1), expiresAt: SampleData.updatedAt)
    try await store.save(expired)
    var speakers = SampleData.speakers()
    speakers[0].sampleClipURL = namedClip
    speakers[1].sampleClipURL = unnamedClip
    #expect(speakers[0].assignment.isConfirmed && !speakers[1].assignment.isConfirmed)
    try await store.replaceTranscript(SampleData.meeting(), segments: [], speakers: speakers)

    var forever = SampleData.meeting()
    forever.id = SampleData.uuid(2)
    try await store.save(forever)
    let keptMaster = try file("kept/master.caf")
    let kept = AudioAsset(
      id: SampleData.uuid(71), meetingID: forever.id, url: keptMaster, format: .caf48kFloat32,
      lanes: [.mixed], retention: .keepForever)
    try await store.save(kept)

    let sweep = RetentionSweep(store: store)
    #expect(try await sweep.run(now: SampleData.updatedAt.addingTimeInterval(-1)).isEmpty)
    let removed = try await sweep.run(now: SampleData.updatedAt)
    #expect(removed == [master, mic, system, mixdown, namedClip], "the confirmed clip goes last")
    for url in removed {
      #expect(!FileManager.default.fileExists(atPath: url.path), "\(url.lastPathComponent)")
    }
    #expect(FileManager.default.fileExists(atPath: unnamedClip.path))
    #expect(FileManager.default.fileExists(atPath: stranger.path))
    #expect(FileManager.default.fileExists(atPath: keptMaster.path))
    let row = try #require(try await store.asset(id: expired.id))
    #expect(row.expiresAt == nil)
    #expect(row.url == master)
    #expect(try await store.asset(id: kept.id) == kept)
    let after = try await store.speakers(meetingID: SampleData.meetingID)
    var expected = speakers
    expected[0].sampleClipURL = nil
    #expect(after == expected, "only the confirmed row loses its clip column")
    #expect(try await sweep.run(now: .distantFuture).isEmpty)
  }

  @Test func missingFilesDoNotAbortTheSweep() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try MeetingStore.inMemory()
    try await store.save(SampleData.meeting())
    let present = directory.appendingPathComponent("present.m4a")
    try Data([1]).write(to: present)
    let asset = AudioAsset(
      id: SampleData.uuid(70), meetingID: SampleData.meetingID,
      url: directory.appendingPathComponent("gone.caf"), format: .caf48kFloat32, lanes: [.mixed],
      sidecars16k: [.mixed: directory.appendingPathComponent("gone.wav")], mixdownURL: present,
      retention: .deleteAfterProcessing, expiresAt: SampleData.updatedAt)
    try await store.save(asset)
    let removed = try await RetentionSweep(store: store).run(now: .distantFuture)
    #expect(removed == [present])
    #expect(try await store.asset(id: asset.id)?.expiresAt == nil)
  }

  @Test func anUndeletableFileIsRetriedNextTimeAndDoesNotBlockOtherAssets() async throws {
    let directory = try Fixtures.temporaryDirectory()
    let locked = directory.appendingPathComponent("locked", isDirectory: true)
    try FileManager.default.createDirectory(at: locked, withIntermediateDirectories: true)
    let stuck = locked.appendingPathComponent("stuck.caf")
    try Data([1]).write(to: stuck)
    let free = directory.appendingPathComponent("free.caf")
    try Data([2]).write(to: free)
    // A read-only parent refuses the unlink; restored before cleanup.
    try FileManager.default.setAttributes([.posixPermissions: 0o500], ofItemAtPath: locked.path)
    defer {
      try? FileManager.default.setAttributes(
        [.posixPermissions: 0o700], ofItemAtPath: locked.path)
      try? FileManager.default.removeItem(at: directory)
    }
    let store = try MeetingStore.inMemory()
    try await store.save(SampleData.meeting())
    var other = SampleData.meeting()
    other.id = SampleData.uuid(2)
    try await store.save(other)
    let stuckAsset = AudioAsset(
      id: SampleData.uuid(70), meetingID: SampleData.meetingID, url: stuck,
      format: .caf48kFloat32, lanes: [.mixed], retention: .deleteAfterProcessing,
      expiresAt: SampleData.updatedAt)
    let freeAsset = AudioAsset(
      id: SampleData.uuid(71), meetingID: other.id, url: free, format: .caf48kFloat32,
      lanes: [.mixed], retention: .deleteAfterProcessing, expiresAt: SampleData.updatedAt)
    try await store.save(stuckAsset)
    try await store.save(freeAsset)

    let sweep = RetentionSweep(store: store)
    let incomplete = await #expect(throws: RetentionSweep.Incomplete.self) {
      try await sweep.run(now: SampleData.updatedAt)
    }
    #expect(incomplete?.failures.map(\.url) == [stuck])
    #expect(!FileManager.default.fileExists(atPath: free.path), "the other asset was swept")
    #expect(try await store.asset(id: freeAsset.id)?.expiresAt == nil)
    #expect(
      try await store.asset(id: stuckAsset.id)?.expiresAt == SampleData.updatedAt,
      "still expired, so the next sweep retries")

    try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: locked.path)
    #expect(try await sweep.run(now: SampleData.updatedAt) == [stuck])
    #expect(try await store.asset(id: stuckAsset.id)?.expiresAt == nil)
  }

  @Test func anUndeletableClipKeepsExpiresAtAndTheClipColumn() async throws {
    let directory = try Fixtures.temporaryDirectory()
    let locked = directory.appendingPathComponent("locked", isDirectory: true)
    try FileManager.default.createDirectory(at: locked, withIntermediateDirectories: true)
    let stuckClip = locked.appendingPathComponent("named.wav")
    try Data([1]).write(to: stuckClip)
    let master = directory.appendingPathComponent("master.caf")
    try Data([2]).write(to: master)
    // A read-only parent refuses the unlink; restored before cleanup.
    try FileManager.default.setAttributes([.posixPermissions: 0o500], ofItemAtPath: locked.path)
    defer {
      try? FileManager.default.setAttributes(
        [.posixPermissions: 0o700], ofItemAtPath: locked.path)
      try? FileManager.default.removeItem(at: directory)
    }
    let store = try MeetingStore.inMemory()
    try await store.save(SampleData.meeting())
    for person in SampleData.persons() { try await store.save(person) }
    let asset = AudioAsset(
      id: SampleData.uuid(70), meetingID: SampleData.meetingID, url: master,
      format: .caf48kFloat32, lanes: [.mixed], retention: .deleteAfterProcessing,
      expiresAt: SampleData.updatedAt)
    try await store.save(asset)
    var speakers = SampleData.speakers()
    speakers[0].sampleClipURL = stuckClip
    speakers[1].sampleClipURL = nil
    try await store.replaceTranscript(SampleData.meeting(), segments: [], speakers: speakers)

    let sweep = RetentionSweep(store: store)
    let incomplete = await #expect(throws: RetentionSweep.Incomplete.self) {
      try await sweep.run(now: SampleData.updatedAt)
    }
    #expect(incomplete?.failures.map(\.url) == [stuckClip])
    #expect(!FileManager.default.fileExists(atPath: master.path), "the audio still went")
    #expect(FileManager.default.fileExists(atPath: stuckClip.path))
    #expect(
      try await store.asset(id: asset.id)?.expiresAt == SampleData.updatedAt,
      "still expired, so the next sweep retries")
    #expect(
      try await store.speakers(meetingID: SampleData.meetingID) == speakers,
      "the clip column waits for the file")

    try FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: locked.path)
    #expect(try await sweep.run(now: SampleData.updatedAt) == [stuckClip])
    #expect(try await store.asset(id: asset.id)?.expiresAt == nil)
    speakers[0].sampleClipURL = nil
    #expect(try await store.speakers(meetingID: SampleData.meetingID) == speakers)
  }

  /// `steno process` stores the mic file as both master and `.mic` sidecar.
  @Test func aSidecarThatIsAlsoTheMasterIsRemovedOnce() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try MeetingStore.inMemory()
    try await store.save(SampleData.meeting())
    let mic = directory.appendingPathComponent("mic.wav")
    let system = directory.appendingPathComponent("system.wav")
    try Data([1]).write(to: mic)
    try Data([2]).write(to: system)
    let asset = AudioAsset(
      id: SampleData.uuid(70), meetingID: SampleData.meetingID, url: mic, format: .wav16kInt16,
      lanes: [.mic, .system], sidecars16k: [.mic: mic, .system: system],
      retention: .deleteAfterProcessing, expiresAt: SampleData.updatedAt)
    try await store.save(asset)
    #expect(asset.expirableFiles == [mic, mic, system])
    let removed = try await RetentionSweep(store: store).run(now: SampleData.updatedAt)
    #expect(removed == [mic, system])
    #expect(!FileManager.default.fileExists(atPath: mic.path))
    #expect(!FileManager.default.fileExists(atPath: system.path))
    #expect(try await store.asset(id: asset.id)?.expiresAt == nil)
  }

  /// A stamp on a meeting that is still recording, queued or processing is
  /// never acted on; the same asset is swept once the meeting settles.
  @Test func aMeetingStillInFlightIsNotSweptUntilItSettles() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try MeetingStore.inMemory()
    try await store.save(SampleData.meeting(state: .processing))
    let master = directory.appendingPathComponent("master.caf")
    try Data([1]).write(to: master)
    let asset = AudioAsset(
      id: SampleData.uuid(70), meetingID: SampleData.meetingID, url: master,
      format: .caf48kFloat32, lanes: [.mixed], retention: .deleteAfterProcessing,
      expiresAt: SampleData.updatedAt)
    try await store.save(asset)
    let sweep = RetentionSweep(store: store)
    for state in [MeetingState.recording, .queued, .processing] {
      try await store.setState(state, meetingID: SampleData.meetingID, now: SampleData.updatedAt)
      #expect(try await store.expiredAssets(now: .distantFuture).isEmpty, "\(state)")
      #expect(try await sweep.run(now: .distantFuture).isEmpty, "\(state)")
      #expect(FileManager.default.fileExists(atPath: master.path), "\(state)")
      #expect(try await store.asset(id: asset.id)?.expiresAt == SampleData.updatedAt, "\(state)")
    }
    try await store.setState(.ready, meetingID: SampleData.meetingID, now: SampleData.updatedAt)
    #expect(try await store.expiredAssets(now: .distantFuture) == [asset])
    #expect(try await sweep.run(now: SampleData.updatedAt) == [master])
    #expect(!FileManager.default.fileExists(atPath: master.path))
    #expect(try await store.asset(id: asset.id)?.expiresAt == nil)

    // A failed meeting settles too: its audio follows the stamp.
    try Data([2]).write(to: master)
    var restamped = asset
    restamped.expiresAt = SampleData.updatedAt
    try await store.save(restamped)
    try await store.setState(
      .failed(reason: "boom"), meetingID: SampleData.meetingID, now: SampleData.updatedAt)
    #expect(try await sweep.run(now: SampleData.updatedAt) == [master])
  }

  /// Switching Settings > Audio to Forever keeps every recording still on
  /// disk: rule and stamp change together, so the next Re-export (the
  /// deferred-case stamp) leaves it alone. An asset whose master is gone is
  /// not rewritten.
  @Test func keepAllRescuesOnlyAssetsWithFiles() async throws {
    let harness = try await PipelineHarness()
    defer { harness.cleanUp() }
    let (meeting, asset) = try harness.meeting(source: .macInPerson)
    try await harness.pipeline.enqueue(meeting, asset: asset)
    await harness.pipeline.waitUntilIdle()
    let stamped = try #require(try await harness.store.asset(id: asset.id))
    #expect(stamped.expiresAt == PipelineHarness.now.addingTimeInterval(30 * 86_400))

    var other = SampleData.meeting()
    other.id = SampleData.uuid(2)
    try await harness.store.save(other)
    let gone = AudioAsset(
      id: SampleData.uuid(71), meetingID: other.id,
      url: harness.directory.appendingPathComponent("gone.caf"), format: .caf48kFloat32,
      lanes: [.mixed], retention: .keepDays(30), expiresAt: PipelineHarness.now)
    try await harness.store.save(gone)

    let sweep = RetentionSweep(store: harness.store)
    #expect(try await sweep.keepAll() == 1)
    let rescued = try #require(try await harness.store.asset(id: asset.id))
    #expect(rescued.retention == .keepForever)
    #expect(rescued.expiresAt == nil)
    #expect(try await harness.store.asset(id: gone.id) == gone, "no file, no rewrite")

    try await harness.pipeline.redeliver(meetingID: meeting.id)
    let afterReexport = try #require(try await harness.store.asset(id: asset.id))
    #expect(afterReexport.retention == .keepForever)
    #expect(afterReexport.expiresAt == nil, "a rescued asset is never restamped")
    #expect(try await sweep.run(now: .distantFuture).isEmpty)
    #expect(FileManager.default.fileExists(atPath: asset.url.path))
    #expect(try await sweep.keepAll() == 1, "idempotent")
  }
}
