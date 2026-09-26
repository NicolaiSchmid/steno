#if canImport(Security)
  import Crypto
  import Foundation
  import Security

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
    /// The label the app and the CLI use.
    public static let defaultLabel = "Steno handover identity"

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
          let update = SecItemUpdate(
            [kSecClass: kSecClassKey, kSecValueRef: key] as CFDictionary,
            [kSecAttrLabel: label, kSecAttrApplicationTag: Data(label.utf8)] as CFDictionary)
          guard update == errSecSuccess else {
            throw IdentityError.security("SecItemUpdate(key)", update)
          }
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
      let relabel = SecItemUpdate(
        [kSecClass: kSecClassCertificate, kSecValueRef: stored] as CFDictionary,
        [kSecAttrLabel: label] as CFDictionary)
      guard relabel == errSecSuccess else {
        throw IdentityError.security("SecItemUpdate(certificate)", relabel)
      }
    }

    /// The identity whose certificate is labelled `label`, or nil when no
    /// certificate carries that label. Never an identity that is not ours:
    /// the certificate is looked up by label (which certificate queries
    /// honour), its label is checked again on the returned attributes, and
    /// the identity built from it must hand back the same certificate.
    /// A labelled certificate without its private key is an error, not nil,
    /// so a half-deleted identity is reported instead of silently replaced
    /// (which would re-pair every phone).
    public static func load(label: String) throws -> SecIdentity? {
      guard let certificate = try storedCertificate(labelled: label) else { return nil }
      var identity: SecIdentity?
      let status = SecIdentityCreateWithCertificate(nil, certificate, &identity)
      guard status == errSecSuccess, let identity else {
        if status == errSecItemNotFound {
          throw IdentityError.malformed(
            "the certificate labelled \"\(label)\" has no private key in the keychain; "
              + "delete it in Keychain Access to mint a new identity")
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

    /// Removes the certificate(s) and the key(s) stored under `label`.
    /// Missing items are not an error. Keys are found through their
    /// certificates first (a key query by label only finds keys this type
    /// labelled itself), then whatever still carries the label goes.
    public static func delete(label: String) throws {
      for certificate in try storedCertificates(labelled: label) {
        var identity: SecIdentity?
        guard SecIdentityCreateWithCertificate(nil, certificate, &identity) == errSecSuccess,
          let identity
        else { continue }
        var key: SecKey?
        guard SecIdentityCopyPrivateKey(identity, &key) == errSecSuccess, let key else { continue }
        let status = SecItemDelete([kSecClass: kSecClassKey, kSecValueRef: key] as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
          throw IdentityError.security("SecItemDelete(key)", status)
        }
      }
      for itemClass in [kSecClassCertificate, kSecClassKey] {
        let status = SecItemDelete([kSecClass: itemClass, kSecAttrLabel: label] as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
          throw IdentityError.security("SecItemDelete", status)
        }
      }
    }

    /// The app and CLI entry point: the stored identity, or a fresh one
    /// minted with `commonName` and stored under `label`. Other identities
    /// in the keychain do not count as ours (#88).
    public static func loadOrCreate(
      label: String = defaultLabel, commonName: String
    ) throws -> HandoverIdentity {
      if let existing = try load(label: label) {
        return try HandoverIdentity(secIdentity: existing)
      }
      try store(try MintedIdentity.mint(commonName: commonName), label: label)
      guard let created = try load(label: label) else {
        throw IdentityError.notFound(label: label)
      }
      return try HandoverIdentity(secIdentity: created)
    }

    // MARK: - Lookups

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

    /// One certificate labelled `label`, with the label verified on the
    /// returned attributes.
    private static func storedCertificate(labelled label: String) throws -> SecCertificate? {
      var result: CFTypeRef?
      let status = SecItemCopyMatching(
        [
          kSecClass: kSecClassCertificate,
          kSecAttrLabel: label,
          kSecReturnRef: true,
          kSecReturnAttributes: true,
          kSecMatchLimit: kSecMatchLimitOne,
        ] as CFDictionary, &result)
      switch status {
      case errSecSuccess:
        guard let attributes = result as? [CFString: Any],
          let reference = attributes[kSecValueRef] as CFTypeRef?,
          CFGetTypeID(reference) == SecCertificateGetTypeID()
        else {
          throw IdentityError.malformed("keychain returned something other than a certificate")
        }
        guard attributes[kSecAttrLabel] as? String == label else {
          throw IdentityError.malformed(
            "keychain returned a certificate labelled "
              + "\"\(attributes[kSecAttrLabel] as? String ?? "")\" for \"\(label)\"")
        }
        return unsafeDowncast(reference, to: SecCertificate.self)
      case errSecItemNotFound:
        return nil
      default:
        throw IdentityError.security("SecItemCopyMatching(certificate)", status)
      }
    }

    /// Every certificate labelled `label`.
    private static func storedCertificates(labelled label: String) throws -> [SecCertificate] {
      var result: CFTypeRef?
      let status = SecItemCopyMatching(
        [
          kSecClass: kSecClassCertificate,
          kSecAttrLabel: label,
          kSecReturnRef: true,
          kSecMatchLimit: kSecMatchLimitAll,
        ] as CFDictionary, &result)
      switch status {
      case errSecSuccess:
        return ((result as? [AnyObject]) ?? [])
          .filter { CFGetTypeID($0) == SecCertificateGetTypeID() }
          .map { unsafeDowncast($0, to: SecCertificate.self) }
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
