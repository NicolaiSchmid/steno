import Foundation
import NIOHTTP1
import StenoCore

/// The protocol core behind the listener: the auth gate and every route,
/// independent of NIO and of TLS so the same code runs on Linux in tests.
/// Owned by `HandoverService`; one actor, so pairing, tokens and partial
/// files are touched by one request at a time. The recording routes live
/// in `RecordingHandler.swift`.
actor HandoverEngine: RequestHandling {
  let configuration: HandoverConfiguration
  let identity: HandoverIdentity
  let store: MeetingStore
  let intake: any HandoverIntake
  let clock: any Clock<Duration>
  let now: @Sendable () -> Date
  nonisolated let inbox: Inbox

  private var pairing: PairingSession?
  /// Receipts touched since start, by recording id; what `receipts` streams.
  var activeReceipts: [UUID: HandoverReceipt] = [:]
  private var receiptUpdates = Broadcast<[HandoverReceipt]>(initial: [])

  /// `lastSeenAt` is written at most this often per device.
  static let lastSeenResolution: TimeInterval = 60

  init(
    configuration: HandoverConfiguration,
    identity: HandoverIdentity,
    store: MeetingStore,
    intake: any HandoverIntake,
    clock: any Clock<Duration>,
    now: @escaping @Sendable () -> Date
  ) {
    self.configuration = configuration
    self.identity = identity
    self.store = store
    self.intake = intake
    self.clock = clock
    self.now = now
    self.inbox = Inbox(directory: configuration.inboxDirectory)
  }

  /// On start, drop inbox files no receipt accounts for (a crash between
  /// announce and the first save, a device revoked while offline) or that a
  /// completed intake left behind (it copied the file before writing
  /// `.complete`). Best effort.
  func sweepOrphans() async {
    try? inbox.prepare()
    for recordingID in inbox.recordingIDs() {
      let receipt = try? await store.handoverReceipt(recordingID: recordingID)
      switch receipt?.state {
      case .none, .complete: inbox.discard(recordingID)
      case .some: break
      }
    }
  }

  // MARK: - Pairing session

  /// Opens a window and returns the payload for the QR code, replacing any
  /// open session.
  func beginPairing() -> PairingPayload {
    let session = PairingSession(
      macID: identity.macID, macName: configuration.serviceName,
      fingerprint: identity.fingerprint, window: configuration.pairingWindow, clock: clock,
      now: now())
    pairing = session
    return session.payload
  }

  func cancelPairing() {
    pairing = nil
  }

  var pairingIsOpen: Bool { pairing?.isOpen ?? false }

  /// Forgets the device and drops whatever it was uploading.
  func revoke(_ deviceID: UUID) async throws {
    for (recordingID, receipt) in activeReceipts where receipt.deviceID == deviceID {
      if receipt.state.kind != .complete {
        inbox.discard(recordingID)
      }
      activeReceipts.removeValue(forKey: recordingID)
    }
    try await store.delete(deviceID: deviceID)
    receiptUpdates.send(receiptsSnapshot)
  }

  // MARK: - Auth gate

  func authenticate(_ route: Route, authorization: String?) async -> AuthOutcome {
    switch route.auth {
    case .none:
      return .allowed(.anonymous)
    case .pairing:
      guard let secret = Self.credential(scheme: "Pairing", in: authorization),
        let session = pairing, session.matches(secret)
      else {
        return .forbidden
      }
      return .allowed(.pairing)
    case .bearer:
      guard let token = Self.credential(scheme: "Bearer", in: authorization) else {
        return .unauthorized
      }
      let hash = DeviceTokens.hash(token)
      guard let device = try? await store.device(forTokenHash: hash) else {
        return .unauthorized
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
    try? await store.save(seen, tokenHash: tokenHash)
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
    case .unpair:
      return await unpair(request)
    case .announce(let recordingID):
      return await announce(recordingID, request)
    case .status(let recordingID):
      return await status(recordingID, request)
    case .chunk(let recordingID, let index):
      return await receiveChunk(recordingID, index: index, request)
    case .complete(let recordingID):
      return await complete(recordingID, request)
    }
  }

  private func pair(_ request: HandoverRequest) async -> HandoverResponse {
    // The gate passed at the head; the window may have closed since.
    guard let session = pairing, session.isOpen else {
      return .problem(.forbidden, "pairing secret rejected")
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
    let token = DeviceTokens.mint()
    let timestamp = now()
    let device = PairedDevice(
      id: body.deviceID, name: name, pairedAt: timestamp, lastSeenAt: timestamp)
    do {
      try await store.save(device, tokenHash: DeviceTokens.hash(token))
    } catch {
      return .problem(.internalServerError, "saving the device failed: \(error)")
    }
    pairing = nil
    return .json(
      .ok,
      Wire.PairResponse(token: token, macID: identity.macID, macName: configuration.serviceName))
  }

  private func unpair(_ request: HandoverRequest) async -> HandoverResponse {
    guard let device = request.device else { return .problem(.unauthorized, "no device") }
    do {
      try await revoke(device.id)
    } catch {
      return .problem(.internalServerError, "revoking failed: \(error)")
    }
    return .empty(.noContent)
  }

  // MARK: - Receipts

  /// Yields the current receipts first, then every change.
  var receipts: AsyncStream<[HandoverReceipt]> {
    receiptUpdates.subscribe { [weak self] id in
      Task { await self?.unsubscribe(id) }
    }
  }

  private func unsubscribe(_ id: UUID) {
    receiptUpdates.remove(id)
  }

  /// Receipts touched since start, oldest first.
  var receiptsSnapshot: [HandoverReceipt] {
    activeReceipts.values.sorted {
      ($0.createdAt, $0.recordingID.uuidString) < ($1.createdAt, $1.recordingID.uuidString)
    }
  }

  /// Writes the receipt and tells the observers.
  func persist(_ receipt: HandoverReceipt) async throws {
    try await store.save(receipt)
    activeReceipts[receipt.recordingID] = receipt
    receiptUpdates.send(receiptsSnapshot)
  }
}
