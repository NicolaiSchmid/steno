#if canImport(Security)
  import Crypto
  import Foundation
  import Security

  /// Stores the minted identity in the file-based login keychain as a
  /// certificate item and a key item, fetched back as `kSecClassIdentity`.
  /// Never `kSecUseDataProtectionKeychain`: that keychain needs an
  /// application-identifier entitlement a Developer ID build and `swift test`
  /// do not have (plan decision 6). Tests touch this only behind
  /// `STENO_KEYCHAIN_TESTS=1`; the CI listener uses `TestIdentity`.
  public enum IdentityKeychain {
    /// The label the app and the CLI use.
    public static let defaultLabel = "Steno handover identity"

    public static func store(_ identity: MintedIdentity, label: String) throws {
      guard let certificate = SecCertificateCreateWithData(nil, identity.certificateDER as CFData)
      else {
        throw IdentityError.malformed("the minted certificate is not DER")
      }
      var error: Unmanaged<CFError>?
      let keyAttributes: [CFString: Any] = [
        kSecAttrKeyType: kSecAttrKeyTypeECSECPrimeRandom,
        kSecAttrKeyClass: kSecAttrKeyClassPrivate,
        kSecAttrKeySizeInBits: 256,
      ]
      guard
        let key = SecKeyCreateWithData(
          identity.privateKey.x963Representation as CFData, keyAttributes as CFDictionary, &error)
      else {
        throw IdentityError.malformed(
          "SecKeyCreateWithData: \(error?.takeRetainedValue().localizedDescription ?? "unknown")")
      }

      let addKey = SecItemAdd(
        [
          kSecClass: kSecClassKey,
          kSecValueRef: key,
          kSecAttrLabel: label,
          kSecAttrApplicationTag: Data(label.utf8),
          kSecAttrIsPermanent: true,
        ] as CFDictionary, nil)
      guard addKey == errSecSuccess || addKey == errSecDuplicateItem else {
        throw IdentityError.security("SecItemAdd(key)", addKey)
      }
      let addCertificate = SecItemAdd(
        [
          kSecClass: kSecClassCertificate,
          kSecValueRef: certificate,
          kSecAttrLabel: label,
        ] as CFDictionary, nil)
      guard addCertificate == errSecSuccess || addCertificate == errSecDuplicateItem else {
        throw IdentityError.security("SecItemAdd(certificate)", addCertificate)
      }
    }

    public static func load(label: String) throws -> SecIdentity? {
      var item: CFTypeRef?
      let status = SecItemCopyMatching(
        [
          kSecClass: kSecClassIdentity,
          kSecAttrLabel: label,
          kSecReturnRef: true,
          kSecMatchLimit: kSecMatchLimitOne,
        ] as CFDictionary, &item)
      switch status {
      case errSecSuccess:
        guard let item, CFGetTypeID(item) == SecIdentityGetTypeID() else {
          throw IdentityError.malformed("keychain returned something other than an identity")
        }
        return unsafeBitCast(item, to: SecIdentity.self)
      case errSecItemNotFound:
        return nil
      default:
        throw IdentityError.security("SecItemCopyMatching(identity)", status)
      }
    }

    /// Removes the certificate and the key stored under `label`. Missing
    /// items are not an error.
    public static func delete(label: String) throws {
      for itemClass in [kSecClassCertificate, kSecClassKey] {
        let status = SecItemDelete([kSecClass: itemClass, kSecAttrLabel: label] as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
          throw IdentityError.security("SecItemDelete", status)
        }
      }
    }

    /// The app and CLI entry point: the stored identity, or a fresh one
    /// minted with `commonName` and stored under `label`.
    public static func loadOrCreate(
      label: String = defaultLabel, commonName: String
    ) throws -> HandoverIdentity {
      if let existing = try load(label: label) {
        return try HandoverIdentity(secIdentity: existing)
      }
      try store(try ServerIdentity.mint(commonName: commonName), label: label)
      guard let created = try load(label: label) else {
        throw IdentityError.notFound(label: label)
      }
      return try HandoverIdentity(secIdentity: created)
    }
  }
#endif
