import Foundation
import StenoCore

/// Where the guard keeps the identity: the login keychain in the product
/// (`IdentityKeychain.Store`), a fake in tests.
public protocol HandoverIdentityStore: Sendable {
  /// The stored identity, nil when none is stored; throws when the store
  /// cannot be read.
  func load() throws -> HandoverIdentity?
  func store(_ identity: MintedIdentity) throws
  /// Removes every stored item of the identity; nothing stored is not an
  /// error.
  func delete() throws
}

/// Why the identity is unavailable rather than minted.
/// Rust: `Unavailability` in `crates/steno-handover/src/identity.rs`.
public enum IdentityUnavailability: Error, CustomStringConvertible, Equatable, Sendable {
  /// The keychain's read failed (a locked or denied keychain).
  case unreadable(String)
  /// The keychain holds no identity, though a fingerprint is recorded or a
  /// phone is paired.
  case missing
  /// The keychain's identity is not the one whose fingerprint is recorded.
  case replaced

  public var description: String {
    switch self {
    case .unreadable(let reason): "the keychain could not be read (\(reason))"
    case .missing: "the pairing your phones know is no longer in this Mac's keychain"
    case .replaced: "the keychain holds another pairing than the one your phones know"
    }
  }
}

/// The handover identity without a silent re-pair: losing the identity
/// makes every paired phone lose this Mac, so nothing is minted while a
/// fingerprint is recorded (`IdentityRecordFile`) or a phone is paired.
/// Rust: `HandoverIdentity::load_or_create` and `HandoverIdentity::pair_again`
/// in `crates/steno-handover/src/identity.rs`.
public struct HandoverIdentityGuard: Sendable {
  public let keychain: any HandoverIdentityStore
  public let record: IdentityRecordFile
  public let store: MeetingStore

  public init(keychain: any HandoverIdentityStore, record: IdentityRecordFile, store: MeetingStore)
  {
    self.keychain = keychain
    self.record = record
    self.store = store
  }

  /// The stored identity; an identity without a record has its fingerprint
  /// recorded. Throws `IdentityUnavailability` when the keychain cannot be
  /// read, holds no identity while a fingerprint is recorded or a phone is
  /// paired, or holds one whose fingerprint is not the recorded one. Only
  /// with no record, no identity and no paired phone is one minted, stored
  /// and recorded.
  public func loadOrCreate(commonName: String) async throws -> HandoverIdentity {
    let recorded = try record.recorded()
    let stored: HandoverIdentity?
    do {
      stored = try keychain.load()
    } catch {
      throw IdentityUnavailability.unreadable(String(describing: error))
    }
    if let stored {
      let fingerprint = ContentHash.hex(stored.fingerprint)
      guard let recorded else {
        try record.record(fingerprint)
        return stored
      }
      guard recorded == fingerprint else { throw IdentityUnavailability.replaced }
      return stored
    }
    if recorded != nil { throw IdentityUnavailability.missing }
    if !(try await store.pairedDevices()).isEmpty { throw IdentityUnavailability.missing }
    return try mint(commonName: commonName)
  }

  /// `loadOrCreate(commonName:)`, and when the identity is missing or
  /// replaced, `askToPairAgain` decides: yes is `pairAgain(commonName:)`,
  /// no rethrows and changes nothing.
  public func loadOrPairAgain(
    commonName: String, askToPairAgain: () async -> Bool
  ) async throws -> HandoverIdentity {
    do {
      return try await loadOrCreate(commonName: commonName)
    } catch let refusal as IdentityUnavailability {
      if case .unreadable = refusal { throw refusal }
      guard await askToPairAgain() else { throw refusal }
      return try await pairAgain(commonName: commonName)
    }
  }

  /// Pair again, the user's choice when the identity the phones know is
  /// gone: every paired phone is removed, the record forgotten and the
  /// keychain's items deleted, then a new identity is minted, stored and
  /// recorded. Every phone must pair again.
  public func pairAgain(commonName: String) async throws -> HandoverIdentity {
    for device in try await store.pairedDevices() {
      try await store.delete(deviceID: device.id)
    }
    try record.forget()
    try keychain.delete()
    return try mint(commonName: commonName)
  }

  private func mint(commonName: String) throws -> HandoverIdentity {
    try keychain.store(try MintedIdentity.mint(commonName: commonName))
    guard let stored = try keychain.load() else {
      throw IdentityError.malformed("the new identity was stored but cannot be read back")
    }
    try record.record(ContentHash.hex(stored.fingerprint))
    return stored
  }
}
