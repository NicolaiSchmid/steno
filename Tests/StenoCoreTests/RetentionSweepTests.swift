import Foundation
import Testing

@testable import StenoCore

@Suite struct RetentionSweepTests {
  @Test func removesMasterSidecarsAndMixdownButNeverClipsOrStrangers() async throws {
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
    let clip = try file("speakers/clip.wav")
    let stranger = try file("notes.txt")
    let expired = AudioAsset(
      id: SampleData.uuid(70), meetingID: SampleData.meetingID, url: master, format: .caf48kFloat32,
      lanes: [.mic, .system], sidecars16k: [.mic: mic, .system: system], mixdownURL: mixdown,
      retention: .keepDays(1), expiresAt: SampleData.updatedAt)
    try await store.save(expired)
    var speaker = SampleData.speakers()[1]
    speaker.sampleClipURL = clip
    try await store.replaceTranscript(
      meetingID: SampleData.meetingID, segments: [], speakers: [speaker])

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
    #expect(removed == [master, mic, system, mixdown])
    for url in [master, mic, system, mixdown] {
      #expect(!FileManager.default.fileExists(atPath: url.path), "\(url.lastPathComponent)")
    }
    #expect(FileManager.default.fileExists(atPath: clip.path))
    #expect(FileManager.default.fileExists(atPath: stranger.path))
    #expect(FileManager.default.fileExists(atPath: keptMaster.path))
    let row = try #require(try await store.asset(id: expired.id))
    #expect(row.expiresAt == nil)
    #expect(row.url == master)
    #expect(try await store.asset(id: kept.id) == kept)
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
}
