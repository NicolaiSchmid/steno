import Crypto
import Foundation
import SwiftASN1
import X509

/// A freshly minted self-signed TLS identity: the certificate's DER bytes and
/// the P-256 key that signed it. Pure value; storing it is `IdentityKeychain`'s
/// job.
public struct MintedIdentity: @unchecked Sendable {
  public let certificateDER: Data
  public let privateKey: P256.Signing.PrivateKey

  public init(certificateDER: Data, privateKey: P256.Signing.PrivateKey) {
    self.certificateDER = certificateDER
    self.privateKey = privateKey
  }

  /// SHA-256 of the leaf DER, the value the phone pins.
  public var fingerprint: Data { ServerIdentity.fingerprint(der: certificateDER) }

  /// The parsed certificate, for tests and diagnostics.
  public func certificate() throws -> Certificate {
    try Certificate(derEncoded: Array(certificateDER))
  }
}

/// Mints the Mac's TLS identity with swift-certificates: P-256, self-signed,
/// ten years, `CN=<commonName>`. No keychain, no Security framework, so the
/// same code runs and is tested on Linux.
public enum ServerIdentity {
  /// Ten years, the whole life of the identity (rotation is a non-goal).
  public static let validity: TimeInterval = 10 * 365 * 24 * 60 * 60

  /// `commonName` is `Steno on <Mac name>` in the product. `now` is the
  /// start of validity; a minute of clock skew is absorbed by backdating.
  public static func mint(commonName: String, now: Date = Date()) throws -> MintedIdentity {
    let privateKey = P256.Signing.PrivateKey()
    let name = try DistinguishedName {
      CommonName(commonName)
    }
    let notBefore = now.addingTimeInterval(-60)
    let certificate = try Certificate(
      version: .v3,
      serialNumber: Certificate.SerialNumber(),
      publicKey: Certificate.PublicKey(privateKey.publicKey),
      notValidBefore: notBefore,
      notValidAfter: notBefore.addingTimeInterval(validity),
      issuer: name,
      subject: name,
      signatureAlgorithm: .ecdsaWithSHA256,
      extensions: try Certificate.Extensions {
        Critical(BasicConstraints.notCertificateAuthority)
        Critical(KeyUsage(digitalSignature: true, keyEncipherment: true))
        try ExtendedKeyUsage([.serverAuth])
        SubjectAlternativeNames([.dnsName(Self.sanLabel(commonName))])
      },
      issuerPrivateKey: Certificate.PrivateKey(privateKey)
    )
    var serializer = DER.Serializer()
    try serializer.serialize(certificate)
    return MintedIdentity(
      certificateDER: Data(serializer.serializedBytes), privateKey: privateKey)
  }

  /// SHA-256 of the certificate DER, the same bytes `SecCertificateCopyData`
  /// yields on the phone.
  public static func fingerprint(der: Data) -> Data {
    Data(SHA256.hash(data: der))
  }

  /// A DNS-safe label for the SAN: the phone never checks the name, but a
  /// certificate without one trips some tooling.
  static func sanLabel(_ commonName: String) -> String {
    let allowed = Set("abcdefghijklmnopqrstuvwxyz0123456789-")
    let label = commonName.lowercased().map { allowed.contains($0) ? String($0) : "-" }.joined()
      .trimmingCharacters(in: CharacterSet(charactersIn: "-"))
    return (label.isEmpty ? "steno" : String(label.prefix(63))) + ".local"
  }
}
