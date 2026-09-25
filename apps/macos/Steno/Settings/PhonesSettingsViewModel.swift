import AppKit
import Foundation
import StenoCore
import StenoHandover

/// Phones: paired devices, the pairing QR code from
/// `HandoverService.beginPairing().urlString`, revoke, listener status and
/// transfers in flight. The listener starts for the first pairing (which
/// triggers the local network prompt) and stays on while phones are paired.
@MainActor
@Observable
final class PhonesSettingsViewModel {
  private(set) var devices: [PairedDevice] = []
  private(set) var listener: ListenerState = .stopped
  private(set) var receipts: [HandoverReceipt] = []
  private(set) var pairing: PairingPayload?
  private(set) var qrImage: NSImage?
  private(set) var error: String?
  let handover: HandoverService?
  private let now: @Sendable () -> Date
  private var observers: [Task<Void, Never>] = []

  init(handover: HandoverService?, now: @escaping @Sendable () -> Date) {
    self.handover = handover
    self.now = now
    guard let handover else { return }
    listener = handover.state
    observers.append(
      Task { [weak self] in
        for await state in handover.states {
          guard let self else { return }
          self.listener = state
        }
      })
    observers.append(
      Task { [weak self] in
        for await receipts in handover.receipts {
          guard let self else { return }
          self.receipts = receipts
        }
      })
  }

  convenience init(environment: AppEnvironment) {
    self.init(handover: environment.handover, now: environment.now)
  }

  var isAvailable: Bool { handover != nil }

  var macID: String? { handover?.identity.macID.uuidString }

  var pairingIsOpen: Bool {
    guard let pairing else { return false }
    return pairing.expiresAt > now()
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
