import Foundation

/// The keychain items an identity is kept in: a certificate found by its
/// label, and the private key the keychain pairs with it by hash. Each is
/// labelled and deleted on its own, so a run cut short can stop between
/// any two writes. `IdentityKeychain.Items` in the product; the tests cut
/// an item-level fake (`IdentityItemsTests`) at every write. The order of
/// the writes lives in this protocol's extension, shared by both.
protocol IdentityItems: Sendable {
  associatedtype Certificate
  associatedtype Key

  /// Every certificate labelled `label`, in the keychain's order: one per
  /// keychain in the search list that holds it (an old login keychain
  /// added back in Keychain Access beside the new one).
  func certificates(labelled label: String) throws -> [Certificate]
  /// `certificate`'s fingerprint, in lowercase hex as the record holds it.
  func fingerprint(of certificate: Certificate) -> String
  /// The identity of `certificate`, which is labelled `label`; throws when
  /// the keychain holds no private key for it.
  func identity(of certificate: Certificate, labelled label: String) throws -> HandoverIdentity
  /// The private key the keychain pairs with `certificate`, nil when it
  /// holds none.
  func privateKey(of certificate: Certificate) -> Key?
  /// Stores `identity`'s key, then its certificate, both under `label`.
  func store(_ identity: MintedIdentity, label: String) throws
  func setLabel(_ label: String, onCertificate certificate: Certificate) throws
  func setLabel(_ label: String, onKey key: Key) throws
  /// Deletes every certificate labelled `label`.
  func deleteCertificates(labelled label: String) throws
  func deleteKey(_ key: Key) throws
  /// Deletes every key labelled `label`.
  func deleteKeys(labelled label: String) throws
}

extension IdentityItems {
  /// The certificate labelled `label` whose fingerprint is `preferred`,
  /// else the keychain's first; nil when none carries the label.
  func certificate(labelled label: String, preferring preferred: String? = nil) throws
    -> Certificate?
  {
    let labelled = try certificates(labelled: label)
    return labelled.first { fingerprint(of: $0) == preferred } ?? labelled.first
  }

  /// The identity whose certificate is `certificate(labelled:preferring:)`,
  /// nil when no certificate carries the label; throws for that certificate
  /// without its private key, so a half-deleted identity is reported
  /// instead of silently replaced.
  func identity(labelled label: String, preferring preferred: String? = nil) throws
    -> HandoverIdentity?
  {
    try certificate(labelled: label, preferring: preferred).map {
      try identity(of: $0, labelled: label)
    }
  }

  /// Removes the certificates and the keys stored under `label`. Missing
  /// items are not an error. The keys are found through their certificates
  /// (a key query by label only finds keys this module labelled), then
  /// whatever still carries the label goes. Certificates go before their
  /// keys, so a run cut short leaves at most a key with no certificate,
  /// which `identity(labelled:)` never finds and the next delete of this
  /// label sweeps; a certificate left without its key would make every
  /// later read throw.
  func delete(label: String) throws {
    let keys = try certificates(labelled: label).compactMap(privateKey(of:))
    try deleteCertificates(labelled: label)
    for key in keys { try deleteKey(key) }
    try deleteKeys(labelled: label)
  }

  /// Stores `identity` under `label` alone: whatever `label` held is
  /// deleted first, and the stored identity is read back.
  func stage(_ identity: MintedIdentity, label: String) throws -> HandoverIdentity {
    try delete(label: label)
    return try storeAndReadBack(identity, label: label)
  }

  /// Stores `identity` under `label` and reads it back.
  func storeAndReadBack(_ identity: MintedIdentity, label: String) throws -> HandoverIdentity {
    try store(identity, label: label)
    guard let stored = try self.identity(labelled: label) else {
      throw IdentityError.notFound(label: label)
    }
    return stored
  }

  /// Gives the identity under `staging` the label `label`: what `label`
  /// holds is deleted, then the staged certificate and its key are
  /// relabelled. The certificate goes first: with only it relabelled,
  /// `identity(labelled: label)` already finds the identity, because the
  /// key is found by its hash, not its label. Throws before anything is
  /// deleted when nothing complete is staged, so `label` is never emptied
  /// for nothing.
  func promote(from staging: String, to label: String) throws {
    guard let certificate = try self.certificate(labelled: staging) else {
      throw IdentityError.notFound(label: staging)
    }
    guard let key = privateKey(of: certificate) else {
      throw IdentityError.malformed(
        "the certificate labelled \"\(staging)\" has no private key in the keychain")
    }
    try delete(label: label)
    try setLabel(label, onCertificate: certificate)
    try setLabel(label, onKey: key)
  }
}

/// The guard's keychain over `items`: the current identity under `label`,
/// a new one staged under `stagingLabel` until it takes `label`.
struct LabelledIdentityKeychain<Items: IdentityItems>: HandoverIdentityKeychain {
  let items: Items
  let label: String
  let stagingLabel: String

  func load(preferring recorded: String?) throws -> HandoverIdentity? {
    try items.identity(labelled: label, preferring: recorded)
  }

  func staged() throws -> HandoverIdentity? { try items.identity(labelled: stagingLabel) }

  func stage(_ identity: MintedIdentity) throws -> HandoverIdentity {
    try items.stage(identity, label: stagingLabel)
  }

  func promote() throws { try items.promote(from: stagingLabel, to: label) }
}
