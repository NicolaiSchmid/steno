import Foundation
import Testing

@testable import StenoHandover

@Suite struct PairingPayloadTests {
  let fingerprint = Data((0..<32).map { UInt8($0 * 7 &+ 3) })
  let secret = Data((0..<32).map { UInt8(255 - $0 * 5) })
  let macID = UUID(uuidString: "6F9619FF-8B86-D011-B42D-00C04FC964FF")!

  @Test func urlStringHasTheWireShapeAndRoundTrips() throws {
    let payload = PairingPayload(
      macID: macID, macName: "Nicolai's MacBook Pro + Office", fingerprint: fingerprint,
      secret: secret, expiresAt: Date(timeIntervalSince1970: 1_790_000_300.7))
    let url = payload.urlString

    #expect(url.hasPrefix("steno://pair/v1?mac=6f9619ff-8b86-d011-b42d-00c04fc964ff&name="))
    #expect(url.contains("&name=Nicolai%27s%20MacBook%20Pro%20%2B%20Office&fp="))
    #expect(url.hasSuffix("&exp=1790000300"))
    let query = url.split(separator: "?")[1]
    #expect(!query.contains("+") && !query.contains("/"), "base64url alphabet only")
    for field in query.split(separator: "&")
    where field.hasPrefix("fp=") || field.hasPrefix("secret=") {
      #expect(!field.hasSuffix("="), "unpadded")
    }

    let parsed = try PairingPayload(parsing: URL(string: url)!)
    #expect(parsed == payload)
    #expect(parsed.expiresAt == Date(timeIntervalSince1970: 1_790_000_300))
    #expect(parsed.macName == "Nicolai's MacBook Pro + Office")
  }

  @Test func base64URLRoundTripsAndRejectsForeignCharacters() {
    for length in [0, 1, 2, 3, 31, 32, 33] {
      let data = Data((0..<length).map { UInt8(truncatingIfNeeded: $0 * 37 + 11) })
      let encoded = Base64URL.encode(data)
      #expect(!encoded.contains("="))
      #expect(Base64URL.decode(encoded) == data)
    }
    #expect(Base64URL.decode("AQID") == Data([1, 2, 3]))
    #expect(Base64URL.decode("AQID==") == Data([1, 2, 3]))
    #expect(Base64URL.decode("AQ+D") == nil, "standard alphabet is not accepted")
    #expect(Base64URL.decode("A") == nil)
    #expect(Base64URL.decode("-_-_") != nil)
  }

  @Test func parseFailuresMatchThePhoneParser() {
    func failure(_ string: String) -> PairingPayloadError? {
      do {
        _ = try PairingPayload(parsing: URL(string: string)!)
        return nil
      } catch let error as PairingPayloadError {
        return error
      } catch {
        return nil
      }
    }
    let good = PairingPayload(
      macID: macID, macName: "Mac", fingerprint: fingerprint, secret: secret,
      expiresAt: Date(timeIntervalSince1970: 1_790_000_300)
    ).urlString
    #expect(failure(good) == nil)
    #expect(failure("https://example.com/pair/v1?mac=1") == .notSteno)
    #expect(failure(good.replacingOccurrences(of: "pair/v1", with: "pair/v2")) == .version)
    #expect(failure(good.replacingOccurrences(of: "&exp=1790000300", with: "")) == .missingField)
    #expect(
      failure(good.replacingOccurrences(of: "&name=Mac", with: "&name=")) == .badEncoding("name"))
    #expect(
      failure(good.replacingOccurrences(of: "&exp=1790000300", with: "&exp=abc"))
        == .badEncoding("exp"))
    #expect(
      failure(
        good.replacingOccurrences(of: "mac=6f9619ff-8b86-d011-b42d-00c04fc964ff", with: "mac=nope"))
        == .badEncoding("mac"))
    let shortFingerprint = good.replacingOccurrences(
      of: "fp=\(Base64URL.encode(fingerprint))",
      with: "fp=\(Base64URL.encode(fingerprint.prefix(31)))")
    #expect(failure(shortFingerprint) == .badEncoding("fp"))
  }
}
