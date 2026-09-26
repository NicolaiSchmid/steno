import AppKit
import Foundation
import StenoCore
import StenoHandover

/// Phones: paired devices, the pairing QR code from
/// `HandoverService.beginPairing().urlString`, revoke, listener status and
/// transfers in flight. The listener starts for the first pairing (which
/// triggers the local network prompt) and stays on while phones are paired.
/// `observe()` and `observePairing()` run from the view's `.task`s; every
/// timer is on the injected clock.
@MainActor
@Observable
final class PhonesSettingsViewModel {
  static let pairingPoll: Duration = .seconds(2)

  private(set) var devices: [PairedDevice] = []
  private(set) var listener: ListenerState = .stopped
  private(set) var receipts: [HandoverReceipt] = []
  private(set) var pairing: PairingPayload?
  private(set) var qrImage: NSImage?
  private(set) var error: String?
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
      self.error = "Paired phones could not be loaded: \(error)"
    }
  }

  /// Starts the listener when needed and opens a pairing window.
  func beginPairing() async {
    guard let handover else { return }
    do {
      try await handover.start()
    } catch {
      self.error = "The listener could not start: \(error)"
      return
    }
    let payload = await handover.beginPairing()
    pairing = payload
    qrImage = QRCode.image(for: payload.urlString)
    error = nil
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
      self.error = "The phone could not be removed: \(error)"
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

  var listenerText: String {
    switch listener {
    case .stopped: "Listener off"
    case .listening(let port): "Listening on port \(port)"
    case .failed(let message): "Listener failed: \(message)"
    }
  }

  static func progress(_ receipt: HandoverReceipt) -> Double {
    guard receipt.byteCount > 0, receipt.chunkSize > 0 else { return 0 }
    let received = Double(receipt.receivedChunks.count) * Double(receipt.chunkSize)
    return min(1, received / Double(receipt.byteCount))
  }
}
