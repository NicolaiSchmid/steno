import Foundation
import StenoCore
import Testing

@testable import StenoHandover

/// The admission ledger (schema v5) over the wire: a recording the computer
/// admitted is answered delivered when it is announced again, whatever
/// happened to its receipt (a revoke, a meeting delete, a line save after the
/// intake's commit, a restart); other bytes under a recording id are a new
/// recording; the same bytes in another split restart the partial; the same
/// bytes from another device take the receipt over, and admitted twice are
/// one meeting; and a late `complete` of replaced bytes never completes the
/// new upload. The intake commits like the
/// real one (`StoreIntake`), so the ledger row exists exactly when an
/// admission committed. The design is
/// `.plans/2026-10-08-handover-admission-ledger.md`. Rust:
/// `crates/steno-handover/tests/admission_ledger.rs`.
@Suite struct AdmissionLedgerTests {
  /// Twice the smallest chunk size, so a resplit can halve it.
  static let chunkSize = 128 * 1024

  /// A started service over `store` whose intake commits like the real one.
  static func service(_ store: MeetingStore, _ intake: StoreIntake) async throws -> TestService {
    let test = try TestService.prepare(chunkSize: chunkSize, store: store, customIntake: intake)
    try await test.service.start()
    return test
  }

  /// `phone` against `test`'s listener: the same token after a restart.
  static func reconnect(_ phone: Phone, to test: TestService) throws -> Phone {
    Phone(
      client: try test.client(), token: phone.token, deviceID: phone.deviceID,
      deviceName: phone.deviceName)
  }

  /// The meeting a `complete` of `recordingID` answered with 200.
  static func completed(_ phone: Phone, _ recordingID: UUID) async throws -> UUID {
    let response = try await phone.complete(recordingID)
    #expect(response.status == 200, "complete")
    return try response.json(Wire.CompleteResponse.self).meetingID
  }

  /// The announce of `metadata` answers 200 `.complete` with every chunk of
  /// its split, and opens no file.
  static func delivered(_ test: TestService, _ phone: Phone, _ metadata: RecordingMetadata)
    async throws
  {
    let announced = try await phone.announce(metadata)
    #expect(announced.status == 200, "announce")
    let count = (Int(metadata.byteCount) + metadata.chunkSize - 1) / metadata.chunkSize
    #expect(
      try announced.json(Wire.RecordingStatus.self)
        == Wire.RecordingStatus(state: .complete, receivedChunks: Array(0..<count)))
    #expect(
      !test.service.engine.inbox.hasPartial(metadata.recordingID), "no partial is opened")
  }

  /// Sends every chunk of `bytes` in `chunkSize`, each answered 204.
  static func sendAll(_ phone: Phone, _ recordingID: UUID, _ bytes: Data, chunkSize: Int)
    async throws
  {
    for (index, chunk) in Phone.chunks(of: bytes, size: chunkSize).enumerated() {
      #expect(try await phone.upload(recordingID, chunk: index, chunk).status == 204)
    }
  }

  @Test func aLostAnswerThenAnUnpairIsAnsweredDeliveredAfterPairingAgain() async throws {
    // The computer admitted the recording, but the 200 of `complete` never
    // reached the phone, so it still holds the row and the file. Then it
    // unpairs, which deletes the receipt with the device. After a restart it
    // pairs again under the same device id and announces the recording: the
    // ledger answers delivered, the phone's `complete` gets the first meeting
    // and deletes its copy, and nothing is admitted twice. A phone under a
    // new device id (an install of the app again) is answered the same once
    // the old one is revoked.
    let store = try MeetingStore.inMemory()
    let intake = StoreIntake(store)
    var test = try await Self.service(store, intake)
    let phone = try await Phone.pair(test.service)
    let bytes = Phone.seededBytes(count: 2 * Self.chunkSize + 1, seed: 31)
    let metadata = phone.metadata(for: bytes, chunkSize: Self.chunkSize)
    let id = metadata.recordingID
    try await phone.uploadAll(metadata, bytes)
    let meetingID = try await Self.completed(phone, id)

    let unpaired = try await phone.client.request("DELETE", "/v1/pairing", headers: phone.bearer)
    #expect(unpaired.status == 204)
    #expect(try await store.handoverReceipt(recordingID: id) == nil, "the revoke cascades")
    await test.stop()

    test = try await Self.service(store, intake)
    let again = try await Phone.pair(
      test.service, deviceID: phone.deviceID, deviceName: phone.deviceName)
    try await Self.delivered(test, again, metadata)
    #expect(try await Self.completed(again, id) == meetingID)
    #expect(intake.meetings == [meetingID], "admitted once")

    try await test.service.revoke(again.deviceID)
    let reinstalled = try await Phone.pair(test.service)
    try await Self.delivered(test, reinstalled, metadata)
    #expect(try await Self.completed(reinstalled, id) == meetingID)
    #expect(intake.meetings == [meetingID])
    #expect(try await store.meetings().count == 1, "one meeting")
    await test.stop()
  }

  @Test func aDeletedMeetingsRecordingIsAnsweredDelivered() async throws {
    // The user deleted the meeting, which deletes its receipt; the phone
    // never got the 200 and announces the recording again. The ledger
    // answers delivered with the deleted meeting's id, so the phone deletes
    // its copy instead of uploading it as a new meeting (Nicolai,
    // 2026-10-07). After a restart, the stored `.complete` receipt whose
    // meeting is gone still reads as admitted.
    let store = try MeetingStore.inMemory()
    let intake = StoreIntake(store)
    var test = try await Self.service(store, intake)
    var phone = try await Phone.pair(test.service)
    let bytes = Phone.seededBytes(count: Self.chunkSize + 5, seed: 32)
    let metadata = phone.metadata(for: bytes, chunkSize: Self.chunkSize)
    let id = metadata.recordingID
    try await phone.uploadAll(metadata, bytes)
    let meetingID = try await Self.completed(phone, id)
    try await store.delete(meetingID: meetingID)
    #expect(try await store.handoverReceipt(recordingID: id) == nil)
    await test.stop()

    test = try await Self.service(store, intake)
    phone = try Self.reconnect(phone, to: test)
    try await Self.delivered(test, phone, metadata)
    #expect(try await Self.completed(phone, id) == meetingID)
    await test.stop()

    test = try await Self.service(store, intake)
    phone = try Self.reconnect(phone, to: test)
    let kept = try await phone.status(id)
    #expect(kept.status == 200)
    #expect(
      try kept.json(Wire.RecordingStatus.self).state == .complete,
      "a complete receipt whose meeting is gone reads as admitted")
    #expect(try await Self.completed(phone, id) == meetingID)
    #expect(intake.meetings == [meetingID], "admitted once")
    #expect(try await store.meetings().isEmpty)
    await test.stop()
  }

  @Test func aReceiptALateSavePutBackReadsAdmittedAfterARestart() async throws {
    // A line save asked for before the intake's commit (a chunk, a
    // re-announce) landed after it and put the stored receipt back to
    // `.verifying`, and the app stopped before the engine's own `.complete`
    // save. After the restart the ledger shows the bytes admitted, so the
    // phone's retried `complete` gets the first meeting, not a second.
    let store = try MeetingStore.inMemory()
    let intake = StoreIntake(store)
    var test = try await Self.service(store, intake)
    var phone = try await Phone.pair(test.service)
    let bytes = Phone.seededBytes(count: Self.chunkSize + 9, seed: 33)
    let metadata = phone.metadata(for: bytes, chunkSize: Self.chunkSize)
    let id = metadata.recordingID
    try await phone.uploadAll(metadata, bytes)
    let meetingID = try await Self.completed(phone, id)
    await test.stop()
    var putBack = try #require(try await store.handoverReceipt(recordingID: id))
    putBack.state = .verifying
    try await store.save(putBack)

    test = try await Self.service(store, intake)
    phone = try Self.reconnect(phone, to: test)
    #expect(try await Self.completed(phone, id) == meetingID)
    #expect(intake.meetings == [meetingID], "admitted once")
    await test.stop()
  }

  @Test func otherBytesUnderAnAdmittedIDAreTakenInAsANewRecording() async throws {
    // The phone announces another file under a recording id the computer
    // admitted (its crash recovery replaced the file). The computer takes it
    // in as a new recording under the same id: a fresh receipt, an empty
    // partial, its own meeting. The phone's `complete` answers 200 only once
    // the new meeting committed, and the ledger then holds both admissions.
    let store = try MeetingStore.inMemory()
    let intake = StoreIntake(store)
    let test = try await Self.service(store, intake)
    let phone = try await Phone.pair(test.service)
    let first = Phone.seededBytes(count: 2 * Self.chunkSize, seed: 34)
    let metadata = phone.metadata(for: first, chunkSize: Self.chunkSize)
    let id = metadata.recordingID
    try await phone.uploadAll(metadata, first)
    let firstMeeting = try await Self.completed(phone, id)

    for (seed, count) in [(UInt64(35), first.count), (UInt64(36), first.count + 7)] {
      let other = Phone.seededBytes(count: count, seed: seed)
      let replaced = phone.metadata(for: other, recordingID: id, chunkSize: Self.chunkSize)
      let announced = try await phone.announce(replaced)
      #expect(announced.status == 201, "a new recording")
      #expect(
        try announced.json(Wire.RecordingStatus.self)
          == Wire.RecordingStatus(state: .receiving, receivedChunks: []))
      try await Self.sendAll(phone, id, other, chunkSize: Self.chunkSize)
      #expect(
        try await store.admittedMeeting(
          recordingID: id, byteCount: replaced.byteCount, sha256: replaced.sha256) == nil)
      let meetingID = try await Self.completed(phone, id)
      #expect(meetingID != firstMeeting, "a meeting of its own")
      #expect(
        try await store.admittedMeeting(
          recordingID: id, byteCount: replaced.byteCount, sha256: replaced.sha256)
          == meetingID, "committed before the 200")
    }
    #expect(
      try await store.admittedMeeting(
        recordingID: id, byteCount: metadata.byteCount, sha256: metadata.sha256)
        == firstMeeting, "the first admission stays in the ledger")
    #expect(intake.meetings.count == 3)

    // The first file announced again (a stale request) is answered from the
    // ledger: both were admitted.
    try await Self.delivered(test, phone, metadata)
    #expect(try await Self.completed(phone, id) == firstMeeting)
    #expect(intake.meetings.count == 3)
    await test.stop()
  }

  @Test func anotherChunkSizeRestartsAnUnfinishedPartial() async throws {
    // The phone announces the same bytes in another split before the receipt
    // is `.complete`: the partial starts over under the new split, no chunk
    // listed, and the upload completes in it.
    let store = try MeetingStore.inMemory()
    let intake = StoreIntake(store)
    let test = try await Self.service(store, intake)
    let phone = try await Phone.pair(test.service)
    let bytes = Phone.seededBytes(count: 2 * Self.chunkSize + 3, seed: 37)
    let metadata = phone.metadata(for: bytes, chunkSize: Self.chunkSize)
    let id = metadata.recordingID
    #expect(try await phone.announce(metadata).status == 201)
    let parts = Phone.chunks(of: bytes, size: Self.chunkSize)
    #expect(try await phone.upload(id, chunk: 0, parts[0]).status == 204)

    var resplit = metadata
    resplit.chunkSize = Self.chunkSize / 2
    let announced = try await phone.announce(resplit)
    #expect(announced.status == 200)
    #expect(
      try announced.json(Wire.RecordingStatus.self)
        == Wire.RecordingStatus(state: .receiving, receivedChunks: []))
    let inbox = test.service.engine.inbox
    #expect(try Data(contentsOf: inbox.partial(id)).isEmpty, "the partial starts over")
    #expect(inbox.loadMetadata(id) == resplit)
    let stored = try #require(try await store.handoverReceipt(recordingID: id))
    #expect(stored.chunkSize == Self.chunkSize / 2)
    #expect(stored.receivedChunks.isEmpty)

    try await Self.sendAll(phone, id, bytes, chunkSize: Self.chunkSize / 2)
    let meetingID = try await Self.completed(phone, id)
    #expect(intake.meetings == [meetingID])
    await test.stop()
  }

  @Test func anotherDeviceTakesOverAnUnfinishedUploadOfTheSameBytes() async throws {
    // The receipt belongs to an older device id of the same phone (it paired
    // again while the computer still held the old pairing). The new device
    // announces the same bytes: it takes the receipt over with the chunks
    // received, and its upload completes. The older device's requests then
    // find no receipt of theirs; its announce of the same bytes takes the
    // `.complete` receipt back and is answered delivered.
    let store = try MeetingStore.inMemory()
    let intake = StoreIntake(store)
    let test = try await Self.service(store, intake)
    let older = try await Phone.pair(test.service)
    let bytes = Phone.seededBytes(count: 3 * Self.chunkSize, seed: 38)
    let metadata = older.metadata(for: bytes, chunkSize: Self.chunkSize)
    let id = metadata.recordingID
    let parts = Phone.chunks(of: bytes, size: Self.chunkSize)
    #expect(try await older.announce(metadata).status == 201)
    #expect(try await older.upload(id, chunk: 0, parts[0]).status == 204)

    let newer = try await Phone.pair(test.service)
    let taken = try await newer.announce(metadata)
    #expect(taken.status == 200, "taken over, not 409")
    #expect(
      try taken.json(Wire.RecordingStatus.self).receivedChunks == [0], "the chunk received stays")
    #expect(try await store.handoverReceipt(recordingID: id)?.deviceID == newer.deviceID)
    #expect(try await older.upload(id, chunk: 1, parts[1]).status == 404)
    for index in 1..<parts.count {
      #expect(try await newer.upload(id, chunk: index, parts[index]).status == 204)
    }
    let meetingID = try await Self.completed(newer, id)

    let back = try await older.announce(metadata)
    #expect(back.status == 200)
    #expect(try back.json(Wire.RecordingStatus.self).state == .complete)
    #expect(try await Self.completed(older, id) == meetingID)
    #expect(intake.meetings == [meetingID], "admitted once")
    await test.stop()
  }

  @Test func aTakeoverDuringTheFirstAdmissionCompletesWithItsMeeting() async throws {
    // The older device's `complete` is in the intake, past its commit, when
    // the newer device announces the same bytes and takes the receipt over:
    // its save puts the newer device's unfinished receipt over the
    // `.complete` one. The partial went to the intake, so the newer device's
    // `complete` finds none and its upload starts over. When it reaches the
    // intake, the ledger holds those bytes, and its `complete` answers the
    // first meeting: the same bytes are one recording, and a second meeting
    // would be a duplicate. Rust:
    // `a_takeover_during_the_first_admission_completes_with_its_meeting`.
    let store = try MeetingStore.inMemory()
    let intake = StoreIntake(store)
    let test = try await Self.service(store, intake)
    let older = try await Phone.pair(test.service)
    let bytes = Phone.seededBytes(count: 2 * Self.chunkSize + 3, seed: 41)
    let metadata = older.metadata(for: bytes, chunkSize: Self.chunkSize)
    let id = metadata.recordingID
    try await older.uploadAll(metadata, bytes)
    let newer = try await Phone.pair(test.service)

    intake.holdNext(.afterTheCommit)
    async let first = older.complete(id)
    await intake.held()
    #expect(try await newer.announce(metadata).status == 200, "taken over")
    intake.release()
    let answer = try await first
    #expect(answer.status == 200)
    let meetingID = try answer.json(Wire.CompleteResponse.self).meetingID
    #expect(
      try await store.handoverReceipt(recordingID: id)?.state == .receiving,
      "the takeover's save landed after the commit")
    #expect(try await newer.complete(id).status == 409, "no partial left")

    try await newer.uploadAll(metadata, bytes)
    #expect(try await Self.completed(newer, id) == meetingID, "the first meeting")
    #expect(intake.meetings == [meetingID], "admitted once")
    #expect(
      try await store.handoverReceipt(recordingID: id)?.state == .complete(meetingID: meetingID))
    try await Self.delivered(test, older, metadata)
    await test.stop()
  }

  @Test func admittedBytesOverAnotherDevicesUnfinishedUploadAreRefused() async throws {
    // The phone's first file under the id was admitted. Another device then
    // announced other bytes under the id, a new recording, and its upload is
    // under way. The phone announces its admitted file again: answered
    // `.complete`, it would replace the receipt of that upload, whose file is
    // still on its way. It is answered 409 instead, that upload is left
    // alone, and it completes as a meeting of its own. Rust:
    // `admitted_bytes_over_another_devices_unfinished_upload_are_refused`.
    let store = try MeetingStore.inMemory()
    let intake = StoreIntake(store)
    let test = try await Self.service(store, intake)
    let phone = try await Phone.pair(test.service)
    let first = Phone.seededBytes(count: 2 * Self.chunkSize, seed: 42)
    let metadata = phone.metadata(for: first, chunkSize: Self.chunkSize)
    let id = metadata.recordingID
    try await phone.uploadAll(metadata, first)
    let firstMeeting = try await Self.completed(phone, id)

    let other = try await Phone.pair(test.service)
    let otherBytes = Phone.seededBytes(count: 2 * Self.chunkSize, seed: 43)
    let theirs = other.metadata(for: otherBytes, recordingID: id, chunkSize: Self.chunkSize)
    let parts = Phone.chunks(of: otherBytes, size: Self.chunkSize)
    #expect(try await other.announce(theirs).status == 201, "a new recording")
    #expect(try await other.upload(id, chunk: 0, parts[0]).status == 204)

    #expect(try await phone.announce(metadata).status == 409)
    let receipt = try #require(try await store.handoverReceipt(recordingID: id))
    #expect(receipt.deviceID == other.deviceID, "the other device's upload stays")
    #expect(receipt.sha256 == theirs.sha256)
    #expect(receipt.state == .receiving)
    #expect(receipt.receivedChunks == [0])
    #expect(test.service.engine.inbox.hasPartial(id), "and so does its partial")

    #expect(try await other.upload(id, chunk: 1, parts[1]).status == 204)
    let theirMeeting = try await Self.completed(other, id)
    #expect(theirMeeting != firstMeeting)
    #expect(intake.meetings == [firstMeeting, theirMeeting])
    await test.stop()
  }

  @Test(arguments: [StoreIntake.Hold.beforeTheCommit, .afterTheCommit])
  func aLateCompleteOfReplacedBytesLeavesTheNewUploadUnfinished(hold: StoreIntake.Hold)
    async throws
  {
    // The phone's `complete` of the first file is in the intake when it
    // announces another file under the id (a new recording). Either the
    // intake refuses the first file, whose receipt is gone, or it committed
    // the first file before the announce. Either way the `complete` write
    // then finds the new upload's receipt in memory and leaves it alone:
    // written into it, the new receipt would read `.complete` with the first
    // file's meeting, and the phone would delete a file the computer does
    // not have. A refused first file's verified copy goes too, so the new
    // upload's `complete` cannot admit it unhashed. The new upload then
    // completes as a meeting of its own. Rust:
    // `a_late_complete_of_replaced_bytes_leaves_the_new_upload_unfinished`.
    let store = try MeetingStore.inMemory()
    let intake = StoreIntake(store)
    let test = try await Self.service(store, intake)
    let phone = try await Phone.pair(test.service)
    let first = Phone.seededBytes(count: 2 * Self.chunkSize, seed: 39)
    let metadata = phone.metadata(for: first, chunkSize: Self.chunkSize)
    let id = metadata.recordingID
    try await phone.uploadAll(metadata, first)
    let other = Phone.seededBytes(count: 2 * Self.chunkSize, seed: 40)
    let replaced = phone.metadata(for: other, recordingID: id, chunkSize: Self.chunkSize)

    intake.holdNext(hold)
    async let late = phone.complete(id)
    await intake.held()
    #expect(try await phone.announce(replaced).status == 201)
    intake.release()
    let answer = try await late
    let firstAdmitted = try await store.admittedMeeting(
      recordingID: id, byteCount: metadata.byteCount, sha256: metadata.sha256)
    switch hold {
    case .beforeTheCommit:
      #expect(answer.status != 200, "the first file's commit is refused")
      #expect(firstAdmitted == nil)
    case .afterTheCommit:
      #expect(answer.status == 200, "the first file was admitted")
      #expect(firstAdmitted == (try answer.json(Wire.CompleteResponse.self).meetingID))
    }

    let receipt = try #require(
      await test.service.engine.receiptsSnapshot.first { $0.recordingID == id })
    #expect(receipt.state == .receiving, "the new upload is unfinished")
    #expect(receipt.sha256 == replaced.sha256)
    #expect(
      try await store.admittedMeeting(
        recordingID: id, byteCount: replaced.byteCount, sha256: replaced.sha256) == nil)
    #expect(try await phone.status(id).json(Wire.RecordingStatus.self).state == .receiving)
    #expect(try await phone.complete(id).status != 200, "nothing to admit yet")

    try await phone.uploadAll(replaced, other)
    let meetingID = try await Self.completed(phone, id)
    #expect(
      try await store.admittedMeeting(
        recordingID: id, byteCount: replaced.byteCount, sha256: replaced.sha256) == meetingID)
    await test.stop()
  }

  @Test func aRecordingAnOlderAppAdmittedIsAnsweredDeliveredAfterTheBackfill() async throws {
    // An older app (this one's `v0.10.0-rc.2`, or a Rust build before v5)
    // admitted the recording: a meeting and a `.complete` receipt, no ledger
    // row. This build's store open backfills the row, so after an unpair and
    // a pairing again the phone's retry is answered delivered, with no
    // second meeting; before it, the retried `complete` is answered from the
    // receipt.
    let directory = try Fixtures.temporaryDirectory("ledger-backfill")
    defer { try? FileManager.default.removeItem(at: directory) }
    let url = directory.appendingPathComponent("steno.sqlite")
    var store = try MeetingStore.onDisk(at: url)
    var test = try TestService.prepare(chunkSize: Self.chunkSize, store: store)
    try await test.service.start()
    var phone = try await Phone.pair(test.service)
    let bytes = Phone.seededBytes(count: Self.chunkSize + 11, seed: 41)
    let metadata = phone.metadata(for: bytes, chunkSize: Self.chunkSize)
    let id = metadata.recordingID
    try await phone.uploadAll(metadata, bytes)
    // The fake intake writes no row; the engine saves the `.complete`
    // receipt itself, and the meeting row is the older intake's.
    let meetingID = try await Self.completed(phone, id)
    try await store.saveAdmittedMeeting(meetingID)
    #expect(
      try await store.admittedMeeting(
        recordingID: id, byteCount: metadata.byteCount, sha256: metadata.sha256) == nil,
      "no ledger row yet")
    await test.stop()

    store = try MeetingStore.onDisk(at: url)
    let intake = StoreIntake(store)
    test = try await Self.service(store, intake)
    phone = try Self.reconnect(phone, to: test)
    #expect(try await Self.completed(phone, id) == meetingID, "the re-sent complete")
    try await test.service.revoke(phone.deviceID)
    phone = try await Phone.pair(
      test.service, deviceID: phone.deviceID, deviceName: phone.deviceName)
    try await Self.delivered(test, phone, metadata)
    #expect(try await Self.completed(phone, id) == meetingID)
    #expect(intake.meetings.isEmpty, "no second admission")
    #expect(try await store.meetings().count == 1)
    await test.stop()
  }
}
