import Foundation
import StenoCore

/// Where the guard keeps the identity: the login keychain in the product
/// (`IdentityKeychain.Store`), a fake in tests. A new identity is staged
/// beside the current one and takes its place last, so a refused write
/// changes nothing.
public protocol HandoverIdentityKeychain: Sendable {
  /// The current identity, nil when none is stored; throws when the
  /// keychain cannot be read. Of several under its label (one per keychain
  /// in the search list, an old login keychain added back), the one whose
  /// fingerprint is `recorded`, else the keychain's first.
  func load(preferring recorded: String?) throws -> HandoverIdentity?
  /// The staged identity, nil when none is staged; throws when it cannot
  /// be read.
  func staged() throws -> HandoverIdentity?
  /// Stages `identity` beside the current one, replacing whatever was
  /// staged, and returns it as read back.
  func stage(_ identity: MintedIdentity) throws -> HandoverIdentity
  /// Makes the staged identity the current one, deleting the current one;
  /// throws, deleting nothing, when nothing is staged.
  func promote() throws
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
    case .unreadable(let reason): "this Mac's phone pairing could not be read (\(reason))"
    case .missing: "the pairing your phones know is no longer in this Mac's keychain"
    case .replaced: "this Mac's keychain holds a different pairing from the one your phones know"
    }
  }
}

/// The handover identity without a silent re-pair: losing the identity
/// makes every paired phone lose this Mac, so nothing is minted while a
/// fingerprint is recorded (`IdentityRecordFile`) or a phone is paired.
/// Rust: `HandoverIdentity::load_or_create` and `HandoverIdentity::mint_anew`
/// in `crates/steno-handover/src/identity.rs`.
public struct HandoverIdentityGuard: Sendable {
  let keychain: any HandoverIdentityKeychain
  let record: IdentityRecordFile
  let store: MeetingStore

  public init(
    keychain: any HandoverIdentityKeychain, record: IdentityRecordFile, store: MeetingStore
  ) {
    self.keychain = keychain
    self.record = record
    self.store = store
  }

  /// The current identity; an identity without a record has its
  /// fingerprint recorded. Throws `IdentityUnavailability` when the
  /// keychain cannot be read, holds no identity while a fingerprint is
  /// recorded or a phone is paired, or holds one whose fingerprint is not
  /// the recorded one. A staged identity is a mint or a Pair Again cut
  /// short: when it is the recorded one it becomes the current one, and
  /// with no record, no identity and no paired phone it is installed. Only
  /// then, with nothing staged, is a new identity minted, staged and
  /// installed.
  public func loadOrCreate(commonName: String) async throws -> HandoverIdentity {
    let recorded = try record.recorded()
    let current: HandoverIdentity?
    do {
      current = try keychain.load(preferring: recorded)
    } catch {
      throw IdentityUnavailability.unreadable(String(describing: error))
    }
    if let current {
      let fingerprint = ContentHash.hex(current.fingerprint)
      guard let recorded else {
        try record.record(fingerprint)
        return current
      }
      if recorded == fingerprint { return current }
    }
    // A staged item that cannot be read is no identity to finish with; the
    // next stage deletes it.
    let staged = try? keychain.staged()
    if let staged, recorded == ContentHash.hex(staged.fingerprint) {
      try keychain.promote()
      return staged
    }
    if current != nil { throw IdentityUnavailability.replaced }
    if recorded != nil { throw IdentityUnavailability.missing }
    if !(try await store.pairedDevices()).isEmpty { throw IdentityUnavailability.missing }
    return try install(staged ?? keychain.stage(try MintedIdentity.mint(commonName: commonName)))
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

  /// Pair Again, the user's choice when the identity the phones know is
  /// gone: a new identity is staged in the keychain, every paired phone is
  /// removed, the record names the new identity, and the staged identity
  /// becomes the current one. Every phone must pair again.
  ///
  /// The keychain write comes first, so a refused one changes nothing. A
  /// run cut short at any step leaves a launch that asks again or finishes
  /// the pairing, never one that mints:
  /// - after the stage, or partway through the phones: the current
  ///   identity and the record are as they were, so the launch is missing
  ///   or replaced again; the staged identity is not the recorded one, so
  ///   it is ignored, and the next stage replaces it;
  /// - after the phones, before the record: the same, with no phone left;
  ///   with no record either (phones paired without one), the launch
  ///   installs the staged identity instead of minting;
  /// - after the record, before the promote: the staged identity is the
  ///   recorded one, so the launch promotes it;
  /// - inside the promote: the default label holds the old identity, its
  ///   key alone, nothing or the new certificate, so the launch loads the
  ///   new identity or promotes it again.
  ///
  /// The record is replaced in one rename, so a cut leaves the old record
  /// or the new one.
  public func pairAgain(commonName: String) async throws -> HandoverIdentity {
    let staged = try keychain.stage(try MintedIdentity.mint(commonName: commonName))
    for device in try await store.pairedDevices() {
      try await store.delete(deviceID: device.id)
    }
    return try install(staged)
  }

  /// Records the staged identity, then makes it the current one.
  private func install(_ staged: HandoverIdentity) throws -> HandoverIdentity {
    try record.record(ContentHash.hex(staged.fingerprint))
    try keychain.promote()
    return staged
  }
}
