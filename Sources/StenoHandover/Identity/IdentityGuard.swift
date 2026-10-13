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
  /// with nothing recorded; a certificate in two keychains of the search
  /// list appears once for each. Throws when the keychain cannot be read or
  /// one has no private key.
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
/// Rust: `Unavailability` in `crates/steno-handover/src/identity.rs`; the
/// plan's Handover note says which rules each app has.
public enum IdentityUnavailability: Error, CustomStringConvertible, Equatable, Sendable {
  /// The keychain's read failed (a locked or denied keychain).
  case unreadable(String)
  /// The keychain holds no identity, though a fingerprint is recorded or a
  /// phone is paired.
  case missing
  /// The keychain's identity is not the one whose fingerprint is recorded,
  /// or, with nothing recorded, no paired phone knows any identity it holds
  /// (`knownByEveryPhone`).
  case replaced
  /// With nothing recorded, some paired phones know one of the keychain's
  /// identities, and the others another one or none. A `replaced` in all
  /// but its words, so the launch asks the same question.
  case partlyReplaced
  /// With nothing recorded, a phone paired, or an identity started, later
  /// than this Mac's clock says it is now: the clock is wrong now or was
  /// then, so the dates cannot say which identity the phones know. The
  /// launch asks nothing and records nothing; once the clock has passed
  /// those times, a launch decides by them again.
  case clockBehind

  public var description: String {
    switch self {
    case .unreadable(let reason): "this Mac's phone pairing could not be read (\(reason))"
    case .missing: "the pairing your phones know is no longer in this Mac's keychain"
    case .replaced: "this Mac's keychain holds a different pairing from the one your phones know"
    case .partlyReplaced:
      "some of your phones were paired before this Mac's pairing changed and can't connect. "
        + "Choose Pair Again the next time Steno opens to reconnect them."
    case .clockBehind:
      "this Mac's date and time are earlier than when its phone pairing was made, so Steno "
        + "can't tell which pairing your phones know. Check the date and time in System "
        + "Settings, then open Steno again."
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
  /// This Mac's clock, read once by a launch with nothing recorded.
  let now: @Sendable () -> Date

  public init(
    keychain: any HandoverIdentityKeychain, record: IdentityRecordFile, store: MeetingStore,
    now: @escaping @Sendable () -> Date = { Date() }
  ) {
    self.keychain = keychain
    self.record = record
    self.store = store
    self.now = now
  }

  /// The current identity. With nothing recorded, the one identity every
  /// paired phone knows has its fingerprint recorded (`adoptable`).
  /// Throws `IdentityUnavailability` when the keychain cannot be read,
  /// holds no identity while a fingerprint is recorded or a phone is
  /// paired, holds one whose fingerprint is not the recorded one, or, with
  /// nothing recorded, none that every paired phone knows, or a phone
  /// paired or an identity started after now (`clockBehind`). A staged
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
  /// `pairAgain(commonName:)`, no rethrows and changes nothing. An
  /// unreadable keychain or a clock behind rethrows without asking.
  public func loadOrPairAgain(
    commonName: String, askToPairAgain: () async -> Bool
  ) async throws -> HandoverIdentity {
    do {
      return try await loadOrCreate(commonName: commonName)
    } catch let refusal as IdentityUnavailability {
      switch refusal {
      // Pair Again mends neither a keychain it cannot read nor a clock, and
      // it would remove phones that know the identity.
      case .unreadable, .clockBehind: throw refusal
      case .missing, .replaced, .partlyReplaced: break
      }
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
  ///   records the keychain's newest identity, since no phone is left to
  ///   predate it, or, with an empty keychain, installs the staged one
  ///   instead of minting;
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
  /// from a build without the record, or a lost record), the one to record
  /// (`knownByEveryPhone`). A certificate in two keychains of the search
  /// list is one identity. Both times come from this Mac's clock, so a
  /// phone paired, or an identity started, after now means the clock is
  /// wrong now or was then: `clockBehind`, and the launch waits. Only this
  /// choice reads the times, so only it waits; a recorded identity loads by
  /// its fingerprint. Nil when the keychain holds none.
  private func adoptable(_ listed: [HandoverIdentity]) async throws -> HandoverIdentity? {
    var fingerprints = Set<Data>()
    let identities = listed.filter { fingerprints.insert($0.fingerprint).inserted }
    guard !identities.isEmpty else { return nil }
    let paired = try await store.pairedDevices().map(\.pairedAt)
    let starts = try identities.map { identity in try read { try identity.notValidBefore() } }
    let now = now()
    if (paired + starts).contains(where: { $0 > now }) { throw IdentityUnavailability.clockBehind }
    return identities[try Self.knownByEveryPhone(starts: starts, paired: paired)]
  }

  /// The pairing-time rule: the index in `starts` of the identity that the
  /// phones paired at `paired` all know. A phone paired before an
  /// identity's start cannot know it, so each phone knows the newest
  /// identity whose start is at or before its pairing; one paired at that
  /// instant counts as paired after it. A minted certificate starts a
  /// minute before its mint, the margin for a pairing right after it. With
  /// no phone paired, the newest identity. Starts compare in whole
  /// seconds, and two identities that start in the same second are no
  /// newest one: a phone whose newest start they share knows neither, so
  /// nothing is chosen silently.
  ///
  /// Throws `replaced` when no phone knows any of them, and
  /// `partlyReplaced` when some do but not all know the same one.
  /// Rust: `HandoverIdentity::known_by_every_phone`.
  static func knownByEveryPhone(starts: [Date], paired: [Date]) throws -> Int {
    let seconds = starts.map { $0.timeIntervalSince1970.rounded(.down) }
    /// The index of the one newest start at or before `instant`.
    func newest(at instant: Date) -> Int? {
      let started = seconds.filter { $0 <= instant.timeIntervalSince1970 }
      guard let start = started.max() else { return nil }
      let tied = seconds.indices.filter { seconds[$0] == start }
      return tied.count == 1 ? tied[0] : nil
    }
    // With no phone paired, the instant after every start.
    let known = (paired.isEmpty ? [.distantFuture] : paired).map(newest(at:))
    if let only = known[0], known.allSatisfy({ $0 == only }) { return only }
    throw known.contains { $0 != nil }
      ? IdentityUnavailability.partlyReplaced : IdentityUnavailability.replaced
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
