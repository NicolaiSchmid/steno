import Foundation
import NIOHTTP1
import StenoCore

/// The protocol core behind the listener: the auth gate and every route.
/// Independent of the channel pipeline and of TLS (it uses NIOHTTP1's value
/// types for headers and status), so `handle` is driven directly by the
/// engine tests and the same code runs on Linux. Owned by `HandoverService`;
/// one actor, so pairing, tokens and partial files are touched by one request
/// at a time, though every store, file and intake call is a suspension point
/// at which the next request runs. Its store writes still commit in the
/// order they were asked for (`inOrder`). The recording routes live in
/// `RecordingHandler.swift`.
actor HandoverEngine: RequestHandling {
  let configuration: HandoverConfiguration
  let identity: HandoverIdentity
  let store: MeetingStore
  let intake: any HandoverIntake
  let now: @Sendable () -> Date
  nonisolated let inbox: Inbox
  /// The service's receipt stream; every persisted change is sent here.
  private nonisolated let receiptUpdates: Broadcast<[HandoverReceipt]>

  private var pairing: PairingSession?
  /// Receipts touched since start, by recording id; what `receipts` streams.
  var activeReceipts: [UUID: HandoverReceipt] = [:]
  /// Recordings whose `complete` is between the `.verifying` write and the
  /// intake's answer. The verify and the admit suspend the actor, so a
  /// retried `complete` must not start a second verify or admission.
  var completing: Set<UUID> = []
  /// Revokes per device since start. A `complete` that sees the count change
  /// across its receipt read and verify admits nothing. Pairing again does
  /// not reset it: the phone pairs again under the same device id.
  var revocations: [UUID: Int] = [:]
  /// Revokes in flight per device. Until a revoke's store delete commits, a
  /// store read still returns the device's receipts, and a `complete` that
  /// starts meanwhile takes the count after the bump, so it is refused
  /// before it reads.
  var revoking: [UUID: Int] = [:]
  /// Revokes started per device since start, never taken back. A pairing
  /// that sees it change across its save keeps `revoked`; `revocations`
  /// could look unchanged there, a failed revoke's undo cancelling the bump
  /// of one that started meanwhile.
  var revokeStarts: [UUID: Int] = [:]
  /// Devices revoked since start and not paired again. Their receipts stay
  /// out of `activeReceipts` (and the stream), also when a request that
  /// read one before the revoke caches or writes it after.
  var revoked: Set<UUID> = []
  /// The store write asked for last (`inOrder`).
  private var lastWrite: Task<Void, any Error>?
  /// Saves one receipt: `store.save`, or in a test a save it can hold on its
  /// way to the store, where two saves could change places.
  private let saveReceipt: @Sendable (HandoverReceipt) async throws -> Void
  /// Writes one chunk into the partial: `ReceivingFile.write`, or in a test
  /// a write it can hold after the bytes landed, before the engine resumes.
  let writeChunk: @Sendable (Data, UInt64, URL) async throws -> Void

  /// `lastSeenAt` is written at most this often per device.
  static let lastSeenResolution: TimeInterval = 60
  /// A partial whose receipt has not moved for this long is abandoned: the
  /// phone that announced it is not coming back, and the sweep reclaims
  /// the space. The receipt stays; a late re-announce starts the upload over.
  static let abandonedAfter: TimeInterval = 14 * 24 * 60 * 60

  /// The two answers of the gate. `unauthorized` is the phone's "the Mac
  /// revoked me" signal; `pairingRejected` a bad, used or expired secret.
  static let unauthorized = HandoverResponse.problem(.unauthorized, "unknown or revoked token")
  static let pairingRejected = HandoverResponse.problem(.forbidden, "pairing secret rejected")

  init(
    configuration: HandoverConfiguration,
    identity: HandoverIdentity,
    store: MeetingStore,
    intake: any HandoverIntake,
    receipts: Broadcast<[HandoverReceipt]>,
    now: @escaping @Sendable () -> Date,
    saveReceipt: (@Sendable (HandoverReceipt) async throws -> Void)? = nil,
    writeChunk: (@Sendable (Data, UInt64, URL) async throws -> Void)? = nil
  ) {
    self.configuration = configuration
    self.identity = identity
    self.store = store
    self.intake = intake
    self.receiptUpdates = receipts
    self.now = now
    self.saveReceipt = saveReceipt ?? { try await store.save($0) }
    self.writeChunk = writeChunk ?? { try await ReceivingFile.write($0, at: $1, to: $2) }
    self.inbox = Inbox(directory: configuration.inboxDirectory)
  }

  /// On start, drop inbox files no receipt accounts for (a crash between
  /// announce and the first save, a device revoked while offline), that a
  /// completed intake left behind (it copied the file before writing
  /// `.complete`), or whose receipt has not moved in `abandonedAfter`. Best
  /// effort.
  func sweepOrphans() async {
    try? inbox.prepare()
    let cutoff = now().addingTimeInterval(-Self.abandonedAfter)
    for recordingID in inbox.recordingIDs() {
      guard let receipt = try? await store.handoverReceipt(recordingID: recordingID) else {
        inbox.discard(recordingID)
        continue
      }
      if receipt.state.kind == .complete || receipt.updatedAt < cutoff {
        inbox.discard(recordingID)
      }
    }
  }

  // MARK: - Pairing session

  /// Opens a window and returns the payload for the QR code, replacing any
  /// open session.
  func beginPairing() -> PairingPayload {
    let session = PairingSession(
      macID: identity.macID, macName: configuration.serviceName,
      fingerprint: identity.fingerprint, window: configuration.pairingWindow, now: now)
    pairing = session
    return session.payload
  }

  func cancelPairing() {
    pairing = nil
  }

  var pairingIsOpen: Bool { pairing?.isOpen ?? false }

  /// Forgets the device and drops whatever it was uploading. The files of
  /// its receipts in memory go here; a `complete` reading a receipt from the
  /// store discards that one's files itself (`revocations`); the rest wait
  /// for the next start's sweep. A `complete` that starts before this
  /// returns is refused (`revoking`), and until the device pairs again its
  /// receipts stay out of memory (`revoked`). A failed delete leaves the
  /// device paired, though the files of its uploads in memory are gone and
  /// a `complete` verifying one of them is refused.
  func revoke(_ deviceID: UUID) async throws {
    // Before the first suspension: a request that starts or checks while
    // the store delete is awaited must already see this revoke.
    revocations[deviceID, default: 0] += 1
    revokeStarts[deviceID, default: 0] += 1
    revoking[deviceID, default: 0] += 1
    revoked.insert(deviceID)
    defer {
      let left = revoking[deviceID, default: 1] - 1
      revoking[deviceID] = left > 0 ? left : nil
    }
    var discarded = false
    for (recordingID, receipt) in activeReceipts where receipt.deviceID == deviceID {
      if receipt.state.kind != .complete {
        inbox.discard(recordingID)
        discarded = true
      }
      activeReceipts.removeValue(forKey: recordingID)
    }
    do {
      try await inOrder { [store] in try await store.delete(deviceID: deviceID) }
    } catch {
      // The delete rolled back, so the device is still paired and no read
      // since the bump was stale. Files discarded above stay gone, though,
      // so then the count stays: a `complete` verifying one must refuse.
      // Another revoke in flight keeps `revoked`.
      if !discarded { revocations[deviceID, default: 1] -= 1 }
      if revoking[deviceID] == 1 { revoked.remove(deviceID) }
      receiptUpdates.send(receiptsSnapshot)
      throw error
    }
    receiptUpdates.send(receiptsSnapshot)
  }

  // MARK: - Auth gate

  /// Runs at the request head, before the body; a rejection carries the
  /// answer the handler writes.
  func authenticate(_ route: Route, authorization: String?) async -> AuthOutcome {
    switch route.auth {
    case .none:
      return .allowed(.anonymous)
    case .pairing:
      guard let secret = Self.credential(scheme: "Pairing", in: authorization),
        let session = pairing, session.matches(secret)
      else {
        return .rejected(Self.pairingRejected)
      }
      return .allowed(.pairing)
    case .bearer:
      guard let token = Self.credential(scheme: "Bearer", in: authorization) else {
        return .rejected(Self.unauthorized)
      }
      let hash = DeviceTokens.hash(token)
      guard let device = try? await store.device(forTokenHash: hash) else {
        return .rejected(Self.unauthorized)
      }
      return .allowed(.device(await touch(device, tokenHash: hash)))
    }
  }

  /// Refreshes `lastSeenAt`, at most once a minute.
  private func touch(_ device: PairedDevice, tokenHash: Data) async -> PairedDevice {
    let timestamp = now()
    if let seen = device.lastSeenAt, timestamp.timeIntervalSince(seen) < Self.lastSeenResolution {
      return device
    }
    var seen = device
    seen.lastSeenAt = timestamp
    try? await inOrder { [store] in
      try await store.touchPairedDevice(id: device.id, tokenHash: tokenHash, seenAt: timestamp)
    }
    return seen
  }

  /// The credential after `<scheme> ` in an `Authorization` header, case
  /// insensitive on the scheme.
  static func credential(scheme: String, in authorization: String?) -> String? {
    guard let authorization else { return nil }
    let parts = authorization.split(separator: " ", maxSplits: 1, omittingEmptySubsequences: true)
    guard parts.count == 2, parts[0].lowercased() == scheme.lowercased() else { return nil }
    let credential = parts[1].trimmingCharacters(in: .whitespaces)
    return credential.isEmpty ? nil : credential
  }

  // MARK: - Routes

  func handle(_ request: HandoverRequest) async -> HandoverResponse {
    switch request.route {
    case .hello:
      return .json(.ok, Wire.Hello(macID: identity.macID))
    case .pair:
      return await pair(request)
    case .unpair, .announce, .status, .chunk, .complete:
      // Every bearer route passed the gate with a device principal.
      guard let device = request.device else { return Self.unauthorized }
      return await handle(request, from: device)
    }
  }

  private func handle(_ request: HandoverRequest, from device: PairedDevice) async
    -> HandoverResponse
  {
    switch request.route {
    case .unpair:
      return await unpair(device)
    case .announce(let recordingID):
      return await announce(recordingID, device: device, body: request.body)
    case .status(let recordingID):
      return await status(recordingID, device: device)
    case .chunk(let recordingID, let index):
      return await receiveChunk(recordingID, index: index, device: device, request)
    case .complete(let recordingID):
      return await complete(recordingID, device: device)
    case .hello, .pair:
      return .problem(.notFound, "no such route")
    }
  }

  private func pair(_ request: HandoverRequest) async -> HandoverResponse {
    // The gate passed at the head; the window may have closed since.
    guard let session = pairing, session.isOpen else {
      return Self.pairingRejected
    }
    let body: Wire.PairRequest
    do {
      body = try StenoJSON.decode(Wire.PairRequest.self, from: request.body)
    } catch {
      return .problem(.badRequest, "PairRequest: \(error)")
    }
    let name = body.deviceName.trimmingCharacters(in: .whitespacesAndNewlines)
    guard !name.isEmpty, name.count <= 128 else {
      return .problem(.badRequest, "deviceName must be 1 to 128 characters")
    }
    // Single use: the session is taken before the first suspension point, so
    // a second request with the same secret that arrives while the save is
    // awaited finds no session and is 403. The window reopens only if the
    // save fails.
    pairing = nil
    let token = DeviceTokens.mint()
    let timestamp = now()
    let device = PairedDevice(
      id: body.deviceID, name: name, pairedAt: timestamp, lastSeenAt: timestamp)
    let revokeStart = revokeStarts[device.id, default: 0]
    do {
      try await inOrder { [store] in
        try await store.save(device, tokenHash: DeviceTokens.hash(token))
      }
    } catch {
      if pairing == nil { pairing = session }
      return .internalError("saving the device", error)
    }
    // A revoke that started during the save deletes the device after it.
    if revokeStarts[device.id, default: 0] == revokeStart { revoked.remove(device.id) }
    return .json(
      .ok,
      Wire.PairResponse(token: token, macID: identity.macID, macName: configuration.serviceName))
  }

  private func unpair(_ device: PairedDevice) async -> HandoverResponse {
    do {
      try await revoke(device.id)
    } catch {
      return .internalError("revoking the device", error)
    }
    return .empty(.noContent)
  }

  // MARK: - Receipts

  /// Receipts touched since start, oldest first.
  var receiptsSnapshot: [HandoverReceipt] {
    activeReceipts.values.sorted {
      ($0.createdAt, $0.recordingID.uuidString) < ($1.createdAt, $1.recordingID.uuidString)
    }
  }

  /// One state change of the receipt memory holds, or of `receipt` when
  /// memory holds none (a revoked device): the state, the chunk set when
  /// given, `updatedAt`, then `persist`. `receipt` comes back changed.
  ///
  /// Two kinds of receipt in memory are left as they are, and nothing is
  /// saved:
  /// - Another device's. A device's write never changes a receipt another
  ///   device announced; a late `complete` of a phone revoked during the
  ///   intake meets one when another device announced the same recording id
  ///   meanwhile. `receipt` comes back as the caller passed it.
  /// - A `.complete` one. A request that read the receipt before the
  ///   phone's `complete` admitted the recording must not put it back, or
  ///   the phone's next `complete` would start over and admit it again.
  ///   `receipt` comes back as memory holds it.
  ///
  /// Callers that answer the phone whatever the write did use `try?`
  /// deliberately: memory already holds the change and the phone's next
  /// request re-reads.
  func transition(
    _ receipt: inout HandoverReceipt, to state: HandoverState, receivedChunks: [Int]? = nil
  ) async throws {
    let held = activeReceipts[receipt.recordingID]
    if let held, held.deviceID != receipt.deviceID { return }
    receipt = held ?? receipt
    if held?.state.kind == .complete { return }
    receipt.state = state
    if let receivedChunks { receipt.receivedChunks = receivedChunks }
    receipt.updatedAt = now()
    try await persist(receipt)
  }

  /// Writes the receipt and tells the observers. Memory (`remember`) is
  /// updated before the awaited save: the actor is reentrant at that
  /// `await`, and the phone keeps two chunks in flight, so the next request
  /// must already see this one's chunk or it would persist a stale copy over
  /// it. The saves commit in that order too (`inOrder`), so the last of a
  /// burst, the one carrying every chunk, is the one the store keeps.
  func persist(_ receipt: HandoverReceipt) async throws {
    remember(receipt)
    try await inOrder { [saveReceipt] in try await saveReceipt(receipt) }
    receiptUpdates.send(receiptsSnapshot)
  }

  /// Runs `write` once every store write asked for before it has returned.
  /// A store call leaves the actor before GRDB's writer queue takes it, so
  /// two writes asked for one after the other could reach the queue, and
  /// commit, the other way round: an older receipt over a newer one, or a
  /// receipt save after the device delete that should have followed it.
  /// Every store write the engine makes itself goes through here; reads do
  /// not wait. A failed write does not hold up the next.
  func inOrder(_ write: @escaping @Sendable () async throws -> Void) async throws {
    let previous = lastWrite
    let task = Task {
      _ = await previous?.result
      try await write()
    }
    lastWrite = task
    try await task.value
  }

  /// Keeps `receipt` as the live copy, unless its device was revoked.
  func remember(_ receipt: HandoverReceipt) {
    if !revoked.contains(receipt.deviceID) {
      activeReceipts[receipt.recordingID] = receipt
    }
  }

  /// Drops the receipt from memory and tells the observers; the store row,
  /// if any, stays.
  func forget(_ recordingID: UUID) {
    activeReceipts.removeValue(forKey: recordingID)
    receiptUpdates.send(receiptsSnapshot)
  }
}
