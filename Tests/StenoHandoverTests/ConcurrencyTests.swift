import Foundation
import StenoCore
import Testing

@testable import StenoHandover

/// The engine is one actor, but every store write, file write and hash is a
/// suspension point at which the next request runs. These tests drive
/// `HandoverEngine.handle` directly (`EngineClient`), so two requests enter
/// the actor in a known order and the races the loopback clients can only
/// make likely are certain.
@Suite struct ConcurrencyTests {
  static let meetingID = UUID(uuidString: "C0C0C0C0-0000-4000-8000-000000000001")!

  @Test func aPairingSecretPairsExactlyOnceUnderConcurrentUse() async throws {
    // Both requests passed the gate (the session was open at both heads);
    // the second must find the session gone, not a save still in flight.
    try await TestService.run(start: false) { test in
      let client = EngineClient(test)
      _ = await test.service.engine.beginPairing()
      let (legitimate, intruder) = (UUID(), UUID())

      async let first = client.pair(deviceID: legitimate, deviceName: "Nicolai's iPhone")
      async let second = client.pair(deviceID: intruder, deviceName: "Photographed QR")
      let statuses = try await [first.code, second.code].sorted()

      #expect(statuses == [200, 403])
      let devices = try await test.service.pairedDevices()
      #expect(devices.count == 1, "one phone paired, not two")
      #expect(await test.service.engine.pairingIsOpen == false, "the secret is spent")
    }
  }

  @Test func concurrentCompletesAdmitOnceAndKeepTheCompleteReceipt() async throws {
    // The phone retries `complete` after its own timeout while the Mac is
    // still copying a large file. A second admission would create a second
    // meeting; with the real intake it can also fail on the moved source and,
    // before the fix, overwrite the `.complete` receipt with `.failed`.
    let intake = ScriptedIntake(
      meetingID: Self.meetingID, failures: 0, delay: .milliseconds(300), admitOnce: true)
    try await TestService.run(chunkSize: 64 * 1024, customIntake: intake, start: false) { test in
      let phone = try await EngineClient.paired(test)
      let bytes = Phone.seededBytes(count: 2 * 64 * 1024, seed: 61)
      let metadata = phone.metadata(for: bytes, chunkSize: 64 * 1024)
      try await phone.uploadAll(metadata, bytes)
      let id = metadata.recordingID

      async let first = phone.complete(id)
      try await Task.sleep(for: .milliseconds(100))
      async let second = phone.complete(id)
      let (a, b) = await (first, second)

      #expect([a.code, b.code].sorted() == [200, 409], "one admits, the retry is told to wait")
      let conflict = try #require([a, b].first { $0.code == 409 })
      #expect(
        try conflict.json(Wire.RecordingStatus.self)
          == Wire.RecordingStatus(state: .verifying, receivedChunks: [0, 1]),
        "the 409 status lists every chunk, so the phone backs off instead of re-uploading")
      #expect(await intake.admissions.count == 1)
      #expect(
        try await test.store.handoverReceipt(recordingID: id)?.state
          == .complete(meetingID: Self.meetingID))

      // The phone's next try after the backoff: same id, no new admission.
      let third = await phone.complete(id)
      #expect(third.code == 200)
      #expect(try third.json(Wire.CompleteResponse.self).meetingID == Self.meetingID)
      #expect(await intake.admissions.count == 1)
    }
  }

  @Test(.timeLimit(.minutes(1)))
  func aReceiptSaveThatReachesTheStoreLateCannotUndoALaterOne() async throws {
    // `store.save` leaves the actor before GRDB's writer queue takes the
    // write, so a save asked for first can reach the store second. Chunk
    // 1's save is held on that way while chunk 0 lands; the store must end
    // with both, as memory does.
    let chunkSize = 64 * 1024
    try await TestService.run(chunkSize: chunkSize, start: false) { test in
      let held = HeldSave(store: test.store)
      let now = test.now
      let engine = HandoverEngine(
        configuration: test.service.configuration, identity: test.service.identity,
        store: test.store, intake: test.intake, receipts: Broadcast(initial: []),
        now: { now }, saveReceipt: held.save)
      _ = await engine.beginPairing()
      let deviceID = UUID()
      #expect(try await EngineClient(engine: engine).pair(deviceID: deviceID).code == 200)
      let phone = EngineDevice(
        engine: engine, device: try #require(try await test.store.pairedDevice(id: deviceID)))
      let bytes = Phone.seededBytes(count: 2 * chunkSize, seed: 62)
      let metadata = phone.metadata(for: bytes, chunkSize: chunkSize)
      let chunks = Phone.chunks(of: bytes, size: chunkSize)
      let id = metadata.recordingID
      #expect(try await phone.announce(metadata).code == 201)

      let first = Task { await phone.upload(id, chunk: 1, chunks[1]) }
      try await until { held.isHolding }
      let second = Task { await phone.upload(id, chunk: 0, chunks[0]) }
      try await until { await engine.activeReceipts[id]?.receivedChunks == [0, 1] }
      // Give chunk 0's save the time an in-memory write takes to overtake.
      let deadline = ContinuousClock.now + .milliseconds(200)
      while held.reachedStore.isEmpty, ContinuousClock.now < deadline {
        try await Task.sleep(for: .milliseconds(1))
      }
      #expect(held.reachedStore.isEmpty, "no receipt save overtakes the held one")
      held.release()

      #expect(await first.value.code == 204)
      #expect(await second.value.code == 204)
      #expect(held.reachedStore == [[1], [0, 1]], "the saves reach the store in order")
      #expect(try await test.store.handoverReceipt(recordingID: id)?.receivedChunks == [0, 1])
    }
  }

  @Test func theGateAnswersWhileAWholeFileHashRuns() async throws {
    // Verifying a 4 GiB upload takes seconds; `/v1/hello` and every other
    // connection's auth gate must not queue behind it on the actor.
    let chunkSize = 16 * 1024 * 1024
    try await TestService.run(chunkSize: chunkSize, start: false) { test in
      let client = EngineClient(test)
      let phone = try await EngineClient.paired(test)
      let bytes = Data(repeating: 0x5A, count: 8 * chunkSize)
      let metadata = phone.metadata(for: bytes, chunkSize: chunkSize)
      try await phone.uploadAll(metadata, bytes)
      let id = metadata.recordingID
      let inbox = test.service.engine.inbox
      let receipts = test.service.receipts

      async let completion = phone.complete(id)
      // `.verifying` is written just before the hash starts.
      for await batch in receipts
      where batch.contains(where: { $0.recordingID == id && $0.state.kind == .verifying }) {
        break
      }
      let hello = await client.hello()
      #expect(hello.code == 200)
      #expect(
        inbox.hasPartial(id) && !inbox.hasVerified(id, format: .m4aAAC),
        "hello was answered while the hash was still running, before the promote")

      let completed = await completion
      #expect(completed.code == 200)
      #expect(await test.intake.admissions.count == 1)
    }
  }
}
