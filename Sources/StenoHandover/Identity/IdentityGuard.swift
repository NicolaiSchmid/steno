import Foundation
import StenoCore

/// Where the guard keeps the identity: the login keychain in the product
/// (`IdentityKeychain.Store`), a fake in tests. A new identity is staged
/// beside the current one and takes its place last, so a refused write
/// changes nothing.
public protocol HandoverIdentityKeychain: Sendable {
  /// The identity whose fingerprint is `recorded`, else the keychain's
  /// first, of several under its label (one per keychain in the search
  /// list, an old login keychain added back); nil when none is stored;
  /// throws when the keychain cannot be read or that identity has no
  /// private key.
  func load(preferring recorded: String) throws -> HandoverIdentity?
  /// Every identity under its label, in the keychain's order, for a launch
  /// with nothing recorded; throws when the keychain cannot be read or one
  /// has no private key.
  func identities() throws -> [HandoverIdentity]
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
/// Rust: `Unavailability` in `crates/steno-handover/src/identity.rs`, which
/// has no `partlyReplaced` and no `pairedAt` rule yet (the plan's Handover
/// note).
public enum IdentityUnavailability: Error, CustomStringConvertible, Equatable, Sendable {
  /// The keychain's read failed (a locked or denied keychain).
  case unreadable(String)
  /// The keychain holds no identity, though a fingerprint is recorded or a
  /// phone is paired.
  case missing
  /// The keychain's identity is not the one whose fingerprint is recorded,
  /// or, with nothing recorded, a paired phone predates every identity it
  /// holds.
  case replaced
  /// With nothing recorded, the keychain's only identity is newer than
  /// some paired phones but not all: those phones cannot know it. A
  /// `replaced` in all but its words, so the launch asks the same question.
  case partlyReplaced

  public var description: String {
    switch self {
    case .unreadable(let reason): "this Mac's phone pairing could not be read (\(reason))"
    case .missing: "the pairing your phones know is no longer in this Mac's keychain"
    case .replaced: "this Mac's keychain holds a different pairing from the one your phones know"
    case .partlyReplaced:
      "Some paired phones were paired before this Mac's current identity and can't connect. "
        + "Pair Again to reconnect them."
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

  /// The current identity. With nothing recorded, the newest identity no
  /// paired phone predates has its fingerprint recorded (`adoptable`).
  /// Throws `IdentityUnavailability` when the keychain cannot be read,
  /// holds no identity while a fingerprint is recorded or a phone is
  /// paired, holds one whose fingerprint is not the recorded one, or, with
  /// nothing recorded, only identities a paired phone predates. A staged
  /// identity is a mint or a Pair Again cut short: when it is the recorded
  /// one it becomes the current one, and with no record, no identity and
  /// no paired phone it is installed. Only then, with nothing staged, is a
  /// new identity minted, staged and installed.
  public func loadOrCreate(commonName: String) async throws -> HandoverIdentity {
    let recorded = try record.recorded()
    let current: HandoverIdentity?
    if let recorded {
      current = try read { try keychain.load(preferring: recorded) }
    } else {
      current = try await adoptable(read { try keychain.identities() })
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
  /// replaced, in full or in part, `askToPairAgain` decides: yes is
  /// `pairAgain(commonName:)`, no rethrows and changes nothing.
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

  /// Of the identities under the label with nothing recorded (an upgrade
  /// from a build without the record, or a lost record), the one to
  /// record: the newest that no paired phone predates, since a phone
  /// paired before an identity's `notValidBefore` cannot know it. A phone
  /// paired at that instant counts as paired after it. Both times come from
  /// this Mac's clock, and a minted certificate starts a minute before its
  /// mint, the margin for a pairing right after it. Nil when the keychain
  /// holds none. Throws `partlyReplaced` when its only identity is newer
  /// than some phones but not all, else `replaced` when a phone predates
  /// every one; with no phone paired, the newest is chosen.
  private func adoptable(_ identities: [HandoverIdentity]) async throws -> HandoverIdentity? {
    guard !identities.isEmpty else { return nil }
    let paired = try await store.pairedDevices().map(\.pairedAt)
    let starts = try identities.map { identity in try read { try identity.notValidBefore() } }
    let earliest = paired.min() ?? .distantFuture
    if let newest = zip(identities, starts).filter({ $0.1 <= earliest }).max(by: { $0.1 < $1.1 }) {
      return newest.0
    }
    if identities.count == 1, let latest = paired.max(), latest >= starts[0] {
      throw IdentityUnavailability.partlyReplaced
    }
    throw IdentityUnavailability.replaced
  }

  /// `body`'s keychain read, any failure as `unreadable`.
  private func read<T>(_ body: () throws -> T) throws -> T {
    do { return try body() } catch {
      throw IdentityUnavailability.unreadable(String(describing: error))
    }
  }

  /// Records the staged identity, then makes it the current one.
  private func install(_ staged: HandoverIdentity) throws -> HandoverIdentity {
    try record.record(ContentHash.hex(staged.fingerprint))
    try keychain.promote()
    return staged
  }
}
