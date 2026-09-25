import Crypto
import Foundation
import StenoCore
import StenoHandover
import Testing

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif

/// The one real-pipeline handover test: the real `HandoverService` on
/// loopback with the committed test identity, a pinned client that pairs and
/// uploads a phone recording in chunks, the real `RecordingIntake` enqueuing
/// it, and a `Meeting` in `.queued` with source `.phone`. No models, no
/// network beyond 127.0.0.1.
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
      store: store, intake: intake, identity: try TestIdentity.load(), clock: ManualClock())
    try await service.start()
    defer { Task { await service.stop() } }

    let baseURL = try #require(await service.loopbackURLForTesting)
    let client = HandoverTestClient(baseURL: baseURL, fingerprint: service.identity.fingerprint)

    // Pair by QR: begin a session, spend the secret for a bearer token.
    let payload = await service.beginPairing()
    let deviceID = UUID()
    let pair = try await client.pair(
      secret: payload.secret,
      request: Wire.PairRequest(deviceID: deviceID, deviceName: "Test iPhone"))
    #expect(pair.macID == service.macID)

    // A phone recording: deterministic bytes standing in for the AAC file
    // (the handover copies and hashes bytes; nothing decodes them here).
    let bytes = seededBytes(count: 1024 * 1024 + 4096, seed: 99)
    let recordingID = UUID()
    let metadata = RecordingMetadata(
      recordingID: recordingID, startedAt: Date(timeIntervalSince1970: 1_789_990_000),
      durationSeconds: 65, byteCount: Int64(bytes.count), sha256: Data(SHA256.hash(data: bytes)),
      chunkSize: 1024 * 1024, format: .m4aAAC, deviceName: "Test iPhone")

    let announce = try await client.announce(metadata, token: pair.token)
    #expect(announce.status == 201)
    let chunks = stride(from: 0, to: bytes.count, by: metadata.chunkSize).map { offset in
      bytes.subdata(in: offset..<min(offset + metadata.chunkSize, bytes.count))
    }
    for (index, chunk) in chunks.enumerated() {
      let response = try await client.uploadChunk(
        recordingID, index: index, chunk, token: pair.token)
      #expect(response.status == 204)
    }
    let complete = try await client.complete(recordingID, token: pair.token)
    #expect(complete.status == 200)
    let meetingID = try StenoJSON.decode(Wire.CompleteResponse.self, from: complete.body).meetingID

    #expect(await enqueued.entries == [meetingID])
    let meeting = try #require(try await store.meeting(id: meetingID))
    #expect(meeting.state == .queued)
    #expect(meeting.source == .phone)
    #expect(meeting.duration == 65)

    // The file landed under the audio folder as the pipeline expects.
    let layout = RecordingLayout(audioFolder: settings.audioFolder, meetingID: meetingID)
    #expect(FileManager.default.fileExists(atPath: layout.master(.m4aAAC).path))
    #expect(try Data(contentsOf: layout.master(.m4aAAC)) == bytes)

    let receipt = try #require(try await store.handoverReceipt(recordingID: recordingID))
    #expect(receipt.state == .complete(meetingID: meetingID))
    #expect(receipt.deviceID == deviceID)

    // Idempotent: a repeat complete returns the same meeting, no new enqueue.
    let again = try await client.complete(recordingID, token: pair.token)
    #expect(
      try StenoJSON.decode(Wire.CompleteResponse.self, from: again.body).meetingID == meetingID)
    #expect(await enqueued.count == 1)
  }

  func seededBytes(count: Int, seed: UInt64) -> Data {
    var state = seed
    var data = Data(capacity: count)
    while data.count < count {
      state &+= 0x9e37_79b9_7f4a_7c15
      var z = state
      z = (z ^ (z >> 30)) &* 0xbf58_476d_1ce4_e5b9
      z = (z ^ (z >> 27)) &* 0x94d0_49bb_1331_11eb
      z ^= z >> 31
      withUnsafeBytes(of: z.littleEndian) { data.append(contentsOf: $0) }
    }
    return data.prefix(count)
  }
}

/// A pinned loopback client for the E2E test: on Apple platforms it pins the
/// leaf fingerprint through the iOS module's evaluator (symlinked into
/// `Support/`); on Linux the listener is plaintext.
struct HandoverTestClient {
  struct Response {
    var status: Int
    var body: Data
  }

  let baseURL: URL
  let fingerprint: Data

  func pair(secret: Data, request: Wire.PairRequest) async throws -> Wire.PairResponse {
    let response = try await send(
      "POST", "/v1/pair", headers: ["Authorization": "Pairing \(secret.base64EncodedString())"],
      body: try StenoJSON.encode(request))
    return try StenoJSON.decode(Wire.PairResponse.self, from: response.body)
  }

  func announce(_ metadata: RecordingMetadata, token: String) async throws -> Response {
    try await send(
      "PUT", "/v1/recordings/\(metadata.recordingID.uuidString)",
      headers: ["Authorization": "Bearer \(token)"], body: try StenoJSON.encode(metadata))
  }

  func uploadChunk(_ recordingID: UUID, index: Int, _ bytes: Data, token: String) async throws
    -> Response
  {
    try await send(
      "PUT", "/v1/recordings/\(recordingID.uuidString)/chunks/\(index)",
      headers: [
        "Authorization": "Bearer \(token)", "Content-Type": "application/octet-stream",
        Wire.chunkHashHeader: Data(SHA256.hash(data: bytes)).base64EncodedString(),
      ], body: bytes)
  }

  func complete(_ recordingID: UUID, token: String) async throws -> Response {
    try await send(
      "POST", "/v1/recordings/\(recordingID.uuidString)/complete",
      headers: ["Authorization": "Bearer \(token)"], body: nil)
  }

  private func send(_ method: String, _ path: String, headers: [String: String], body: Data?)
    async throws -> Response
  {
    var request = URLRequest(url: baseURL.appendingPathComponent(path))
    request.httpMethod = method
    request.timeoutInterval = 20
    for (name, value) in headers { request.setValue(value, forHTTPHeaderField: name) }
    if let body { request.httpBody = body }
    let session = makeSession()
    defer { session.finishTasksAndInvalidate() }
    let (data, response) = try await session.data(for: request)
    guard let http = response as? HTTPURLResponse else { throw ClientError.notHTTP }
    return Response(status: http.statusCode, body: data)
  }

  private func makeSession() -> URLSession {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.timeoutIntervalForRequest = 20
    #if canImport(Security)
      return URLSession(
        configuration: configuration, delegate: PinningDelegate(fingerprint: fingerprint),
        delegateQueue: nil)
    #else
      return URLSession(configuration: configuration)
    #endif
  }

  enum ClientError: Error { case notListening, notHTTP }
}

#if canImport(Security)
  import Security

  final class PinningDelegate: NSObject, URLSessionTaskDelegate {
    let fingerprint: Data
    init(fingerprint: Data) { self.fingerprint = fingerprint }

    func urlSession(
      _ session: URLSession, didReceive challenge: URLAuthenticationChallenge,
      completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
      let (disposition, credential) = PinnedTrustEvaluator.respond(
        to: challenge, pinnedFingerprint: fingerprint)
      completionHandler(disposition, credential)
    }

    func urlSession(
      _ session: URLSession, task: URLSessionTask,
      willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest,
      completionHandler: @escaping (URLRequest?) -> Void
    ) {
      completionHandler(nil)
    }
  }
#endif
