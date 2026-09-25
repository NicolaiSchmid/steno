import Crypto
import Foundation

#if canImport(Security)
  import Security
#endif

/// The identity the listener presents: the leaf certificate (for the
/// fingerprint the phone pins and the Mac id derived from it) and, on Apple
/// platforms, the `SecIdentity` Network.framework terminates TLS with. Read
/// it as mint (`MintedIdentity.mint`), store (`IdentityKeychain`), load
/// (this type).
///
/// On Linux there is no TLS stack in this module (Network.framework and
/// Security do not exist there); the identity is the certificate alone and
/// `HandoverServer` listens in plaintext on loopback so the protocol core can
/// be built and tested in a container. That path never compiles into the Mac
/// product.
public struct HandoverIdentity: @unchecked Sendable {
  public let certificateDER: Data

  #if canImport(Security)
    public let secIdentity: SecIdentity

    /// Reads the certificate out of `secIdentity`.
    public init(secIdentity: SecIdentity) throws {
      var certificate: SecCertificate?
      let status = SecIdentityCopyCertificate(secIdentity, &certificate)
      guard status == errSecSuccess, let certificate else {
        throw IdentityError.security("SecIdentityCopyCertificate", status)
      }
      self.secIdentity = secIdentity
      self.certificateDER = SecCertificateCopyData(certificate) as Data
    }
  #else
    public init(certificateDER: Data) {
      self.certificateDER = certificateDER
    }
  #endif

  /// SHA-256 of the leaf DER, the value the phone pins.
  public var fingerprint: Data { Self.fingerprint(ofDER: certificateDER) }

  /// The stable id of this Mac, derived from the certificate: a new identity
  /// is a new Mac to every phone, which matches "losing the identity means
  /// re-pairing".
  public var macID: UUID { Self.macID(forFingerprint: fingerprint) }

  /// SHA-256 of a certificate's DER, the same bytes `SecCertificateCopyData`
  /// yields on the phone (`PinnedTrustEvaluator.fingerprint(of:)`).
  public static func fingerprint(ofDER der: Data) -> Data {
    Data(SHA256.hash(data: der))
  }

  /// The Mac id in the Bonjour TXT record, the QR payload and `/v1/hello`:
  /// a UUID from the first 16 bytes of SHA-256("steno-mac-id" || fingerprint)
  /// with version 4 and variant 1 bits set.
  public static func macID(forFingerprint fingerprint: Data) -> UUID {
    let digest = Self.fingerprint(ofDER: Data("steno-mac-id".utf8) + fingerprint)
    var bytes = Array(digest.prefix(16))
    bytes[6] = (bytes[6] & 0x0F) | 0x40
    bytes[8] = (bytes[8] & 0x3F) | 0x80
    return UUID(
      uuid: (
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8],
        bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
      ))
  }
}

public enum IdentityError: Error, CustomStringConvertible, Sendable {
  /// A Security framework call failed with the given `OSStatus`.
  case security(String, Int32)
  case notFound(label: String)
  case malformed(String)

  public var description: String {
    switch self {
    case .security(let call, let status): "\(call) failed with OSStatus \(status)"
    case .notFound(let label): "no identity labelled \(label) in the keychain"
    case .malformed(let what): what
    }
  }
}
