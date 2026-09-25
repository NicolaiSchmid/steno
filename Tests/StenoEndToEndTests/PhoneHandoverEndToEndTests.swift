import Foundation
import StenoCore
import Testing

@testable import StenoHandover

/// The one real-pipeline handover test: the real `HandoverService` on
/// loopback with the committed test identity, a pinned client (`Phone` over
/// `LoopbackClient`, shared with `StenoHandoverTests` through `Support/`
/// symlinks) that pairs and uploads a recording in chunks, the real
/// `RecordingIntake` enqueuing it, and a `Meeting` in `.queued` with source
/// `.phone`. No models, no network beyond 127.0.0.1.
@Suite struct PhoneHandoverEndToEndTests {
  @Test func testPhoneUploadBecomesQueuedMeeting() async throws {
    let directory = try Fixtures.temporaryDirectory("handover-e2e")
    defer { try? FileManager.default.removeItem(at: directory) }

    let store = try MeetingStore.inMemory()
    let settingsStore = SettingsStore(writer: store.writer)
    var settings = Settings()
    settings.audioFolder = directory.appendingPathComponent("audio", isDirectory: true)
    settings.defaultRetention = .keepDays(30)
    try await settingsStore.save(settings)

    // The real intake, wired the way the CLI and app wire it: enqueue is the
    // pipeline's persist-and-run. Here it saves the meeting and asset so the
    // meeting is observable and the intake stays idempotent.
    let enqueued = CallLog<UUID>()
    let intake = RecordingIntake(
      store: store, settings: settingsStore,
      enqueue: { meeting, asset in
        try await store.save(meeting, asset: asset)
        await enqueued.record(meeting.id)
      })

    let service = HandoverService(
      configuration: HandoverConfiguration(
        serviceName: "Steno on Test Mac", advertise: false, chunkSize: 1024 * 1024,
        inboxDirectory: directory.appendingPathComponent("inbox", isDirectory: true)),
      store: store, intake: intake, identity: try TestIdentity.load())
    try await service.start()
    defer { Task { await service.stop() } }

    // Pair by QR (the secret becomes a bearer token), then upload a phone
    // recording: deterministic bytes standing in for the AAC file, because
    // the handover copies and hashes bytes and nothing decodes them here.
    let phone = try await Phone.pair(service)
    let bytes = Phone.seededBytes(count: 1024 * 1024 + 4096, seed: 99)
    let metadata = phone.metadata(for: bytes)
    try await phone.uploadAll(metadata, bytes)
    let complete = try await phone.complete(metadata.recordingID)
    #expect(complete.status == 200)
    let meetingID = try complete.json(Wire.CompleteResponse.self).meetingID

    #expect(await enqueued.entries == [meetingID])
    let meeting = try #require(try await store.meeting(id: meetingID))
    #expect(meeting.state == .queued)
    #expect(meeting.source == .phone)
    #expect(meeting.duration == metadata.durationSeconds)

    // The file landed under the audio folder as the pipeline expects.
    let layout = RecordingLayout(audioFolder: settings.audioFolder, meetingID: meetingID)
    #expect(try Data(contentsOf: layout.master(.m4aAAC)) == bytes)

    let receipt = try #require(
      try await store.handoverReceipt(recordingID: metadata.recordingID))
    #expect(receipt.state == .complete(meetingID: meetingID))
    #expect(receipt.deviceID == phone.deviceID)

    // Idempotent: a repeat complete returns the same meeting, no new enqueue.
    let again = try await phone.complete(metadata.recordingID)
    #expect(try again.json(Wire.CompleteResponse.self).meetingID == meetingID)
    #expect(await enqueued.count == 1)
  }
}
