#if canImport(Security)
  import Crypto
  import Foundation
  import Security
  import StenoCore

  /// Stores the minted identity in the file-based login keychain as a
  /// certificate item and a key item, and reads it back as a `SecIdentity`.
  /// Never `kSecUseDataProtectionKeychain`: that keychain needs an
  /// application-identifier entitlement a Developer ID build and `swift test`
  /// do not have (plan decision 6). Tests touch this only behind
  /// `STENO_KEYCHAIN_TESTS=1`; the CI listener uses `TestIdentity`.
  ///
  /// Two properties of the file keychain shape this type (#88, verified on
  /// macOS 26.7):
  ///
  /// - `SecItemAdd` of a `kSecClassKey` by `kSecValueRef` is routed to the
  ///   data-protection keychain (`errSecMissingEntitlement`, -34018) because
  ///   a `SecKeyCreateWithData` key is not a CDSA key. The one API that puts
  ///   a software-minted key into the file keychain is `SecItemImport` of
  ///   its SEC1 DER into a `SecKeychain`.
  /// - `SecItemCopyMatching` with `kSecClassIdentity` ignores `kSecAttrLabel`
  ///   and returns every identity in the keychain (MDM identities included).
  ///   Only certificate and key queries honour the label, so every lookup
  ///   here goes through the certificate and `SecIdentityCreateWithCertificate`.
  public enum IdentityKeychain {
    /// The label of the app's identity, the one the Rust app's import reads.
    public static let defaultLabel = "Steno handover identity"
    /// The label a new identity is stored under until it takes
    /// `defaultLabel` (`Store.stage`, `Store.promote`). A leftover is
    /// harmless: `load(label: defaultLabel)` and the Rust app's import match
    /// the label exactly. It is cleared only by the next stage (a Pair Again
    /// or a first mint), never at launch: after a promote cut short between
    /// its two relabels, the current identity's key still carries this
    /// label.
    public static let stagingLabel = "Steno handover identity (new)"

    public static func store(_ identity: MintedIdentity, label: String) throws {
      guard let certificate = SecCertificateCreateWithData(nil, identity.certificateDER as CFData)
      else {
        throw IdentityError.malformed("the minted certificate is not DER")
      }
      let keychain = try defaultKeychain()

      // The key: RFC 5915 DER through the OpenSSL importer (#88). Anything
      // else (PKCS#8, `SecItemAdd` of a key ref) never reaches the file
      // keychain.
      var format = SecExternalFormat.formatOpenSSL
      var itemType = SecExternalItemType.itemTypePrivateKey
      var parameters = SecItemImportExportKeyParameters()
      parameters.version = UInt32(SEC_KEY_IMPORT_EXPORT_PARAMS_VERSION)
      var imported: CFArray?
      let importStatus = SecItemImport(
        try identity.privateKeySEC1DER() as CFData, nil, &format, &itemType, [], &parameters,
        keychain, &imported)
      switch importStatus {
      case errSecSuccess:
        // The importer names the key after nothing; the label and tag make
        // it recognisable in Keychain Access and deletable by label.
        if let key = (imported as? [AnyObject])?.first, CFGetTypeID(key) == SecKeyGetTypeID() {
          try setLabel(label, on: unsafeDowncast(key, to: SecKey.self))
        }
      case errSecDuplicateItem:
        break
      default:
        throw IdentityError.security("SecItemImport(key)", importStatus)
      }

      // The certificate. `kSecUseKeychain` is deprecated along with the
      // whole file keychain API and has no replacement there (#88); it
      // keeps the certificate beside the key.
      let addStatus = SecItemAdd(
        [
          kSecClass: kSecClassCertificate,
          kSecValueRef: certificate,
          kSecUseKeychain: keychain,
        ] as CFDictionary, nil)
      guard addStatus == errSecSuccess || addStatus == errSecDuplicateItem else {
        throw IdentityError.security("SecItemAdd(certificate)", addStatus)
      }
      // The file keychain labels a certificate with its subject CN on add
      // and ignores `kSecAttrLabel` in the add dictionary, so the label is
      // set afterwards on the stored item (found by DER, which also covers
      // the duplicate case).
      guard let stored = try storedCertificate(matchingDER: identity.certificateDER) else {
        throw IdentityError.malformed("the certificate was added but cannot be found again")
      }
      try setLabel(label, on: stored)
    }

    /// The identity whose certificate is labelled `label`, or nil when no
    /// certificate carries that label; of several (one per keychain in the
    /// search list), the first. Never an identity that is not ours: the
    /// certificate is looked up by label (which certificate queries honour),
    /// its label is checked again on the returned attributes, and the
    /// identity built from it must hand back the same certificate. A
    /// labelled certificate without its private key is an error, not nil, so
    /// a half-deleted identity is reported instead of silently replaced
    /// (which would re-pair every phone). The guard's `Store` prefers the
    /// recorded one of several (`IdentityItems.identity(labelled:preferring:)`)
    /// and, with none recorded, lists them all for the guard to choose
    /// (`LabelledIdentityKeychain.identities`).
    public static func load(label: String) throws -> SecIdentity? {
      try Items().certificate(labelled: label).map { try secIdentity(of: $0, labelled: label) }
    }

    /// The identity the keychain pairs with `certificate`, which is
    /// labelled `label`, checked to hand back the same certificate.
    private static func secIdentity(of certificate: SecCertificate, labelled label: String)
      throws -> SecIdentity
    {
      var identity: SecIdentity?
      let status = SecIdentityCreateWithCertificate(nil, certificate, &identity)
      guard status == errSecSuccess, let identity else {
        if status == errSecItemNotFound {
          throw IdentityError.malformed(
            "the certificate labelled \"\(label)\" has no private key in the keychain; "
              + "delete it in Keychain Access, then open Steno again")
        }
        throw IdentityError.security("SecIdentityCreateWithCertificate", status)
      }
      var identityCertificate: SecCertificate?
      let copy = SecIdentityCopyCertificate(identity, &identityCertificate)
      guard copy == errSecSuccess, let identityCertificate else {
        throw IdentityError.security("SecIdentityCopyCertificate", copy)
      }
      guard
        SecCertificateCopyData(identityCertificate) as Data
          == SecCertificateCopyData(certificate) as Data
      else {
        throw IdentityError.malformed(
          "the keychain paired the certificate labelled \"\(label)\" with another identity")
      }
      return identity
    }

    /// `IdentityItems.delete(label:)` in the login keychain.
    public static func delete(label: String) throws {
      try Items().delete(label: label)
    }

    /// The CLI's entry point (`steno dev handover serve`, under its own
    /// label): the stored identity, or a fresh one minted with `commonName`
    /// and stored under `label`, with no guard. Other identities in the
    /// keychain do not count as ours (#88). The app goes through
    /// `HandoverIdentityGuard`, which never mints over a record or a paired
    /// phone.
    public static func loadOrCreate(
      label: String = defaultLabel, commonName: String
    ) throws -> HandoverIdentity {
      let items = Items()
      if let existing = try items.identity(labelled: label) { return existing }
      return try items.storeAndReadBack(
        try MintedIdentity.mint(commonName: commonName), label: label)
    }

    /// The items labelled `defaultLabel`, with `stagingLabel` for a new
    /// identity, as the guard's keychain (`HandoverIdentityGuard`).
    public struct Store: HandoverIdentityKeychain {
      let keychain: any HandoverIdentityKeychain

      public init() {
        self.init(
          keychain: LabelledIdentityKeychain(
            items: Items(), label: defaultLabel, stagingLabel: stagingLabel))
      }

      /// Over `keychain` in place of the login keychain's items.
      init(keychain: any HandoverIdentityKeychain) { self.keychain = keychain }

      public func load(preferring recorded: String) throws -> HandoverIdentity? {
        try keychain.load(preferring: recorded)
      }

      public func identities() throws -> [HandoverIdentity] { try keychain.identities() }

      public func staged() throws -> HandoverIdentity? { try keychain.staged() }

      public func stage(_ identity: MintedIdentity) throws -> HandoverIdentity {
        try keychain.stage(identity)
      }

      public func promote() throws { try keychain.promote() }
    }

    /// The login keychain's certificate and key items, through the lookups
    /// and writes of this type.
    struct Items: IdentityItems {
      func certificates(labelled label: String) throws -> [SecCertificate] {
        try storedCertificates(labelled: label)
      }

      func fingerprint(of certificate: SecCertificate) -> String {
        ContentHash.hex(
          HandoverIdentity.fingerprint(ofDER: SecCertificateCopyData(certificate) as Data))
      }

      func identity(of certificate: SecCertificate, labelled label: String) throws
        -> HandoverIdentity
      {
        try HandoverIdentity(secIdentity: secIdentity(of: certificate, labelled: label))
      }

      func privateKey(of certificate: SecCertificate) -> SecKey? {
        IdentityKeychain.privateKey(of: certificate)
      }

      func store(_ identity: MintedIdentity, label: String) throws {
        try IdentityKeychain.store(identity, label: label)
      }

      func setLabel(_ label: String, onCertificate certificate: SecCertificate) throws {
        try IdentityKeychain.setLabel(label, on: certificate)
      }

      func setLabel(_ label: String, onKey key: SecKey) throws {
        try IdentityKeychain.setLabel(label, on: key)
      }

      func deleteCertificates(labelled label: String) throws {
        try deleteAll(kSecClassCertificate, labelled: label)
      }

      func deleteKey(_ key: SecKey) throws {
        let status = SecItemDelete([kSecClass: kSecClassKey, kSecValueRef: key] as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
          throw IdentityError.security("SecItemDelete(key)", status)
        }
      }

      func deleteKeys(labelled label: String) throws {
        try deleteAll(kSecClassKey, labelled: label)
      }

      /// The file keychain deletes one item per `SecItemDelete` unless the
      /// query says `kSecMatchLimitAll`; without it a second labelled
      /// certificate would survive the delete.
      private func deleteAll(_ itemClass: CFString, labelled label: String) throws {
        let status = SecItemDelete(
          [kSecClass: itemClass, kSecAttrLabel: label, kSecMatchLimit: kSecMatchLimitAll]
            as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
          throw IdentityError.security("SecItemDelete", status)
        }
      }
    }

    // MARK: - Lookups

    /// The private key the keychain pairs with `certificate`, found by its
    /// hash; nil when it holds none.
    private static func privateKey(of certificate: SecCertificate) -> SecKey? {
      var identity: SecIdentity?
      guard SecIdentityCreateWithCertificate(nil, certificate, &identity) == errSecSuccess,
        let identity
      else { return nil }
      var key: SecKey?
      guard SecIdentityCopyPrivateKey(identity, &key) == errSecSuccess else { return nil }
      return key
    }

    /// Labels and tags `key` with `label`.
    private static func setLabel(_ label: String, on key: SecKey) throws {
      let status = SecItemUpdate(
        [kSecClass: kSecClassKey, kSecValueRef: key] as CFDictionary,
        [kSecAttrLabel: label, kSecAttrApplicationTag: Data(label.utf8)] as CFDictionary)
      guard status == errSecSuccess else {
        throw IdentityError.security("SecItemUpdate(key)", status)
      }
    }

    private static func setLabel(_ label: String, on certificate: SecCertificate) throws {
      let status = SecItemUpdate(
        [kSecClass: kSecClassCertificate, kSecValueRef: certificate] as CFDictionary,
        [kSecAttrLabel: label] as CFDictionary)
      guard status == errSecSuccess else {
        throw IdentityError.security("SecItemUpdate(certificate)", status)
      }
    }

    /// The default file keychain (the login keychain in the product).
    /// `SecKeychainCopyDefault` is deprecated with no replacement for the
    /// file-based keychain, the only one this type may use (#88).
    private static func defaultKeychain() throws -> SecKeychain {
      var keychain: SecKeychain?
      let status = SecKeychainCopyDefault(&keychain)
      guard status == errSecSuccess, let keychain else {
        throw IdentityError.security("SecKeychainCopyDefault", status)
      }
      return keychain
    }

    /// Every certificate labelled `label`, in the keychain's order across
    /// the search list, with the label verified on each one's returned
    /// attributes: a certificate under another label is an error, never
    /// ours.
    private static func storedCertificates(labelled label: String) throws -> [SecCertificate] {
      var result: CFTypeRef?
      let status = SecItemCopyMatching(
        [
          kSecClass: kSecClassCertificate,
          kSecAttrLabel: label,
          kSecReturnRef: true,
          kSecReturnAttributes: true,
          kSecMatchLimit: kSecMatchLimitAll,
        ] as CFDictionary, &result)
      switch status {
      case errSecSuccess:
        guard let matches = result as? [[CFString: Any]] else {
          throw IdentityError.malformed("keychain returned something other than certificates")
        }
        return try matches.map { attributes in
          guard let reference = attributes[kSecValueRef] as CFTypeRef?,
            CFGetTypeID(reference) == SecCertificateGetTypeID()
          else {
            throw IdentityError.malformed("keychain returned something other than a certificate")
          }
          let found = attributes[kSecAttrLabel] as? String
          guard found == label else {
            throw IdentityError.malformed(
              "keychain returned a certificate labelled \"\(found ?? "")\" for \"\(label)\"")
          }
          return unsafeDowncast(reference, to: SecCertificate.self)
        }
      case errSecItemNotFound:
        return []
      default:
        throw IdentityError.security("SecItemCopyMatching(certificates)", status)
      }
    }

    /// The stored certificate item whose bytes equal `der`, or nil.
    private static func storedCertificate(matchingDER der: Data) throws -> SecCertificate? {
      var result: CFTypeRef?
      let status = SecItemCopyMatching(
        [
          kSecClass: kSecClassCertificate,
          kSecReturnRef: true,
          kSecMatchLimit: kSecMatchLimitAll,
        ] as CFDictionary, &result)
      switch status {
      case errSecSuccess:
        return ((result as? [AnyObject]) ?? [])
          .filter { CFGetTypeID($0) == SecCertificateGetTypeID() }
          .map { unsafeDowncast($0, to: SecCertificate.self) }
          .first { SecCertificateCopyData($0) as Data == der }
      case errSecItemNotFound:
        return nil
      default:
        throw IdentityError.security("SecItemCopyMatching(certificates)", status)
      }
    }
  }
#endif
