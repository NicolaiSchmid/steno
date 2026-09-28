import AppKit
import Foundation
import StenoCore
import StenoHandover

/// iPhone: paired devices, the pairing QR code from
/// `HandoverService.beginPairing().urlString`, revoke, and transfers in
/// flight. The listener starts for the first pairing (which triggers the
/// local network prompt) and stays on while phones are paired; its port and
/// the Mac's id are not shown. `observe()` and `observePairing()` run from
/// the view's `.task`s; every timer is on the injected clock.
@MainActor
@Observable
final class PhonesSettingsViewModel: SettingsSectionModel {
  static let pairingPoll: Duration = .seconds(2)

  private(set) var devices: [PairedDevice] = []
  private(set) var listener: ListenerState = .stopped
  private(set) var receipts: [HandoverReceipt] = []
  private(set) var pairing: PairingPayload?
  private(set) var qrImage: NSImage?
  var error: String?
  var errorDetails: String?
  let handover: HandoverService?
  private let now: @Sendable () -> Date
  private let clock: any Clock<Duration>

  init(
    handover: HandoverService?, now: @escaping @Sendable () -> Date,
    clock: any Clock<Duration> = ContinuousClock()
  ) {
    self.handover = handover
    self.now = now
    self.clock = clock
    if let handover { listener = handover.state }
  }

  convenience init(environment: AppEnvironment) {
    self.init(handover: environment.handover, now: environment.now, clock: environment.clock)
  }

  var isAvailable: Bool { handover != nil }

  var macID: String? { handover?.identity.macID.uuidString }

  var pairingIsOpen: Bool {
    guard let pairing else { return false }
    return pairing.expiresAt > now()
  }

  /// "It expires in 4 minutes."
  var pairingExpiryText: String? {
    guard let pairing else { return nil }
    let seconds = max(0, pairing.expiresAt.timeIntervalSince(now()))
    let minutes = Int((seconds / 60).rounded(.up))
    return minutes <= 1 ? "It expires in a minute." : "It expires in \(minutes) minutes."
  }

  /// Transfers still arriving, oldest first.
  var activeReceipts: [HandoverReceipt] {
    receipts.filter { $0.state.kind == .receiving || $0.state.kind == .verifying }
      .sorted { $0.createdAt < $1.createdAt }
  }

  /// The paired phone a transfer comes from, by name.
  func deviceName(for receipt: HandoverReceipt) -> String {
    devices.first { $0.id == receipt.deviceID }?.name ?? "your iPhone"
  }

  /// "Paired 12 Sep 2026 · last seen today at 10:06".
  func pairedText(_ device: PairedDevice) -> String {
    var text = "Paired \(device.pairedAt.formatted(date: .abbreviated, time: .omitted))"
    if let seen = device.lastSeenAt {
      text += " · last seen \(seen.formatted(date: .abbreviated, time: .shortened))"
    }
    return text
  }

  /// Follows the listener state until cancelled (one view `.task`).
  func observe() async {
    guard let handover else { return }
    for await state in handover.states {
      listener = state
    }
  }

  /// Follows the transfers in flight until cancelled (a second `.task`).
  func observeReceipts() async {
    guard let handover else { return }
    for await update in handover.receipts {
      receipts = update
    }
  }

  /// While a pairing code is shown, polls the paired devices on the clock so
  /// the phone's arrival closes the code; ends when the code closes or the
  /// task is cancelled.
  func observePairing() async {
    while !Task.isCancelled, pairingIsOpen {
      do {
        try await clock.sleep(for: Self.pairingPoll)
      } catch {
        return
      }
      await refreshAfterPairing()
    }
  }

  func load() async {
    guard let handover else { return }
    do {
      devices = try await handover.pairedDevices()
    } catch {
      fail("Paired phones could not be loaded.", error)
    }
  }

  /// Starts the listener when needed and opens a pairing window.
  func beginPairing() async {
    guard let handover else { return }
    do {
      try await handover.start()
    } catch {
      fail("Pairing could not start.", error)
      return
    }
    let payload = await handover.beginPairing()
    pairing = payload
    qrImage = QRCode.image(for: payload.urlString)
    clearError()
  }

  func cancelPairing() async {
    guard let handover else { return }
    await handover.cancelPairing()
    pairing = nil
    qrImage = nil
    await load()
    if devices.isEmpty { await handover.stop() }
  }

  func revoke(_ deviceID: UUID) async {
    guard let handover else { return }
    do {
      try await handover.revoke(deviceID)
      await load()
      if devices.isEmpty, pairing == nil { await handover.stop() }
    } catch {
      fail("The phone could not be removed.", error)
    }
  }

  /// A device appearing in the list closes the pairing window.
  func refreshAfterPairing() async {
    let before = Set(devices.map(\.id))
    await load()
    if Set(devices.map(\.id)) != before {
      pairing = nil
      qrImage = nil
    }
  }

  /// The listener's failure, if any, for the details disclosure.
  var listenerFailure: String? {
    if case .failed(let message) = listener { return message }
    return nil
  }

  static func progress(_ receipt: HandoverReceipt) -> Double {
    guard receipt.byteCount > 0, receipt.chunkSize > 0 else { return 0 }
    let received = Double(receipt.receivedChunks.count) * Double(receipt.chunkSize)
    return min(1, received / Double(receipt.byteCount))
  }
}
