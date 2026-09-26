import Foundation
import GRDB
import Testing

@testable import StenoCore

@Suite struct MeetingDeletionTests {
  /// Writes a small file at `url`, creating its folder.
  static func touch(_ url: URL) throws {
    try FileManager.default.createDirectory(
      at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
    try Data([1, 2, 3]).write(to: url)
  }

  /// A second meeting with one segment, so the FTS tables are never empty by
  /// accident.
  static func saveBystander(in store: MeetingStore) async throws -> Meeting {
    var other = SampleData.meeting()
    other.id = SampleData.uuid(2)
    other.calendarEventID = nil
    try await store.save(other)
    let speaker = Speaker(
      id: SampleData.uuid(25), meetingID: other.id, clusterLabel: "Speaker 1",
      clusterConfidence: 0.5)
    let segment = TranscriptSegment(
      id: SampleData.uuid(45), meetingID: other.id, start: 0, end: 1, speakerID: speaker.id,
      lane: .mixed, text: "Unbeteiligt.", rawText: "unbeteiligt")
    try await store.replaceTranscript(other, segments: [segment], speakers: [speaker])
    return other
  }

  static func rowCount(_ store: MeetingStore, table: String, meetingID: UUID? = nil) async throws
    -> Int
  {
    try await store.writer.read { db in
      if let meetingID {
        return try Int.fetchOne(
          db, sql: "SELECT count(*) FROM \(table) WHERE meetingID = ?",
          arguments: [meetingID.uuidString]) ?? -1
      }
      return try Int.fetchOne(db, sql: "SELECT count(*) FROM \(table)") ?? -1
    }
  }

  @Test func deleteRemovesRowsReceiptFolderAndPostsTheEvent() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try await MeetingStoreTests.populated()
    let layout = RecordingLayout(audioFolder: directory, meetingID: SampleData.meetingID)
    let clip = layout.sampleClip(speakerID: SampleData.speakerTwoID)
    for url in [
      layout.master(.caf48kFloat32), layout.sidecar(.mic), layout.sidecar(.system),
      layout.mixdown(.m4aAAC), clip,
    ] {
      try Self.touch(url)
    }
    var asset = SampleData.audioAsset()
    asset.url = layout.master(.caf48kFloat32)
    asset.sidecars16k = [.mic: layout.sidecar(.mic), .system: layout.sidecar(.system)]
    asset.mixdownURL = layout.mixdown(.m4aAAC)
    try await store.save(asset)
    var speakers = SampleData.speakers()
    speakers[1].sampleClipURL = clip
    try await store.replaceTranscript(
      SampleData.meeting(), segments: SampleData.segments(), speakers: speakers)
    try await store.save(SampleData.delivery())
    try await store.save(SampleData.pairedDevice(), tokenHash: Data(repeating: 1, count: 32))
    try await store.save(SampleData.handoverReceipt())
    let other = try await Self.saveBystander(in: store)
    let stranger = directory.appendingPathComponent("notes.txt")
    try Self.touch(stranger)
    let events = await store.events.subscribe()

    try await store.delete(meetingID: SampleData.meetingID)

    #expect(try await store.meeting(id: SampleData.meetingID) == nil)
    for table in [
      "participant", "speaker", "transcriptSegment", "meetingTask", "decision", "audioAsset",
      "delivery",
    ] {
      #expect(
        try await Self.rowCount(store, table: table, meetingID: SampleData.meetingID) == 0,
        "\(table)")
    }
    #expect(try await Self.rowCount(store, table: "transcriptSegment_ft") == 1)
    #expect(try await Self.rowCount(store, table: "meeting_ft") == 1)
    #expect(try await store.search("neunzig").isEmpty)
    #expect(try await store.search("Zeitplan").map(\.meetingID) == [other.id])
    #expect(try await store.search("Unbeteiligt").map(\.meetingID) == [other.id])
    #expect(try await store.handoverReceipt(recordingID: SampleData.uuid(91)) == nil)
    #expect(try await store.pairedDevices().count == 1, "the phone stays paired")
    #expect(try await store.persons().count == 2, "persons belong to every meeting")
    #expect(try await store.speakers(meetingID: other.id).count == 1)
    #expect(!FileManager.default.fileExists(atPath: layout.directory.path), "the folder is gone")
    #expect(FileManager.default.fileExists(atPath: stranger.path), "the audio folder itself stays")

    var iterator = events.makeAsyncIterator()
    #expect(await iterator.next() == .deleted(meetingID: SampleData.meetingID))
  }

  @Test(arguments: [MeetingState.recording, .processing])
  func deleteRefusesAMeetingWhoseFilesAreInUse(state: MeetingState) async throws {
    let store = try MeetingStore.inMemory()
    try await store.save(SampleData.meeting(state: state), asset: SampleData.audioAsset())
    await #expect(throws: MeetingStoreError.meetingBusy(SampleData.meetingID, state.kind)) {
      try await store.delete(meetingID: SampleData.meetingID)
    }
    #expect(try await store.meeting(id: SampleData.meetingID)?.state == state)
    #expect(try await store.asset(id: SampleData.uuid(70)) != nil)
    #expect(
      MeetingStoreError.meetingBusy(SampleData.meetingID, state.kind).description.contains(
        state.kind.rawValue))
  }

  @Test(arguments: [MeetingState.queued, .ready, .failed(reason: "boom")])
  func deleteAcceptsEveryOtherState(state: MeetingState) async throws {
    let store = try MeetingStore.inMemory()
    try await store.save(SampleData.meeting(state: state))
    try await store.delete(meetingID: SampleData.meetingID)
    #expect(try await store.meeting(id: SampleData.meetingID) == nil)
  }

  @Test func deleteOfAnUnknownMeetingThrowsAndPostsNothing() async throws {
    let store = try MeetingStore.inMemory()
    let events = await store.events.subscribe()
    await #expect(throws: MeetingStoreError.meetingNotFound(SampleData.uuid(999))) {
      try await store.delete(meetingID: SampleData.uuid(999))
    }
    let collected = await store.events.drain(events)
    #expect(collected.isEmpty)
  }

  @Test func anAssetInASharedFolderLosesOnlyItsOwnFiles() async throws {
    let directory = try Fixtures.temporaryDirectory()
    defer { try? FileManager.default.removeItem(at: directory) }
    let store = try MeetingStore.inMemory()
    try await store.save(SampleData.meeting())
    for person in SampleData.persons() { try await store.save(person) }
    let master = directory.appendingPathComponent("master.wav")
    let mic = directory.appendingPathComponent("mic.wav")
    let clip = directory.appendingPathComponent("speakers/clip.wav")
    let neighbour = directory.appendingPathComponent("other-meeting.wav")
    for url in [master, mic, clip, neighbour] { try Self.touch(url) }
    // The master doubles as the mic sidecar, as `steno process` stores it.
    let asset = AudioAsset(
      id: SampleData.uuid(70), meetingID: SampleData.meetingID, url: master, format: .wav16kInt16,
      lanes: [.mic, .system], sidecars16k: [.mic: master, .system: mic],
      retention: .keepDays(1))
    try await store.save(asset)
    var speaker = SampleData.speakers()[1]
    speaker.sampleClipURL = clip
    try await store.replaceTranscript(SampleData.meeting(), segments: [], speakers: [speaker])
    #expect(
      MeetingStore.filesToRemove(meetingID: SampleData.meetingID, assets: [asset], clips: [clip])
        == [master, mic, clip])

    try await store.delete(meetingID: SampleData.meetingID)

    for url in [master, mic, clip] {
      #expect(!FileManager.default.fileExists(atPath: url.path), "\(url.lastPathComponent)")
    }
    #expect(FileManager.default.fileExists(atPath: neighbour.path))
    #expect(FileManager.default.fileExists(atPath: directory.path))
  }

  @Test func aFailedRecordingWithoutAnAssetIsJustARow() async throws {
    let store = try MeetingStore.inMemory()
    try await store.save(SampleData.meeting(state: .failed(reason: "interrupted")))
    try await store.save(SampleData.participants()[0])
    try await store.delete(meetingID: SampleData.meetingID)
    #expect(try await store.meetings().isEmpty)
    #expect(try await Self.rowCount(store, table: "participant") == 0)
    #expect(
      MeetingStore.filesToRemove(meetingID: SampleData.meetingID, assets: [], clips: []).isEmpty)
  }

  @Test func aFileThatResistsIsThrownAfterTheRowsAreGone() async throws {
    let directory = try Fixtures.temporaryDirectory()
    let locked = directory.appendingPathComponent("locked", isDirectory: true)
    let stuck = locked.appendingPathComponent("stuck.caf")
    try Self.touch(stuck)
    try FileManager.default.setAttributes([.posixPermissions: 0o500], ofItemAtPath: locked.path)
    defer {
      try? FileManager.default.setAttributes(
        [.posixPermissions: 0o700], ofItemAtPath: locked.path)
      try? FileManager.default.removeItem(at: directory)
    }
    let store = try MeetingStore.inMemory()
    let asset = AudioAsset(
      id: SampleData.uuid(70), meetingID: SampleData.meetingID, url: stuck,
      format: .caf48kFloat32, lanes: [.mixed], retention: .keepForever)
    try await store.save(SampleData.meeting(), asset: asset)
    let events = await store.events.subscribe()

    await #expect(throws: (any Error).self) {
      try await store.delete(meetingID: SampleData.meetingID)
    }
    #expect(try await store.meeting(id: SampleData.meetingID) == nil, "the rows went first")
    #expect(FileManager.default.fileExists(atPath: stuck.path))
    var iterator = events.makeAsyncIterator()
    #expect(await iterator.next() == .deleted(meetingID: SampleData.meetingID))
  }
}
