import Foundation
import StenoCore
import Testing

@testable import StenoHandover

/// The phone's half of the wire, read as text from `mobile/` and held against
/// what the Mac encodes and decodes: the field names of every JSON body, the
/// value kinds the phone's `decodeShape` demands, the enum values, the
/// constants, the seven paths and the QR query names. The iOS side runs the
/// mirror image in `native-contract.test.ts`, so a rename on either side
/// fails one CI or the other.
@Suite struct WireContractTests {
  static let repository = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()  // StenoHandoverTests
    .deletingLastPathComponent()  // Tests
    .deletingLastPathComponent()  // repository root

  static func phoneSource(_ relative: String) throws -> String {
    try String(contentsOf: repository.appendingPathComponent(relative), encoding: .utf8)
  }

  let wire: String
  let pairing: String
  let recorder: String

  init() throws {
    wire = try Self.phoneSource("mobile/modules/steno-link/src/wire.ts")
    pairing = try Self.phoneSource("mobile/src/features/pairing/pairing-payload.ts")
    recorder = try Self.phoneSource("mobile/src/features/recorder/recording-options.ts")
  }

  // MARK: - Parsing the TypeScript

  /// Capture groups of every match, in source order.
  static func captures(_ pattern: String, in source: String) throws -> [[String]] {
    let regex = try Regex(pattern).dotMatchesNewlines()
    return source.matches(of: regex).map { match in
      (1..<match.output.count).map { String(match.output[$0].substring ?? "") }
    }
  }

  static func first(_ pattern: String, in source: String) throws -> String? {
    try captures(pattern, in: source).first?.first
  }

  /// `export type Name = { a: string; b: number[] }` → `["a", "b"]`.
  func objectFields(_ name: String) throws -> [String] {
    let body = try #require(
      try Self.first(#"export type \#(name) = \{([^}]*)\}"#, in: wire), "no object type \(name)")
    return try Self.captures(#"(\w+)\??:\s"#, in: body).map(\.[0])
  }

  /// `export type Name = "a" | "b";` → `["a", "b"]`.
  func unionValues(_ name: String) throws -> [String] {
    let body = try #require(try Self.first(#"export type \#(name) =([^;]*);"#, in: wire))
    return try Self.captures(#""([^"]+)""#, in: body).map(\.[0])
  }

  /// `decodeShape<Name>(text, { a: "string", b: "number[]" })` → the shape.
  func decodeShape(_ name: String) throws -> [String: String] {
    let body = try #require(
      try Self.first(#"decodeShape<\#(name)>\(text,\s*\{([^}]*)\}"#, in: wire),
      "no decodeShape for \(name)")
    var shape: [String: String] = [:]
    for pair in try Self.captures(#"(\w+):\s*"([^"]+)""#, in: body) {
      shape[pair[0]] = pair[1]
    }
    return shape
  }

  // MARK: - The Mac's encoding

  static func encoded<T: Encodable>(_ value: T) throws -> [String: Any] {
    try #require(
      JSONSerialization.jsonObject(with: StenoJSON.encode(value)) as? [String: Any],
      "\(T.self) does not encode as a JSON object")
  }

  /// `JSONSerialization` yields `NSNumber` on both platforms (an `NSNumber`
  /// holding 0 or 1 also bridges to `Bool`, so no `Bool` test here; the wire
  /// carries no booleans). The phone's check is `typeof x === "number"`.
  static func isNumber(_ value: Any) -> Bool {
    value is NSNumber || value is any BinaryInteger || value is any BinaryFloatingPoint
  }

  static func isInteger(_ value: Any) -> Bool {
    if value is any BinaryInteger { return true }
    if let number = value as? NSNumber { return number.doubleValue == number.doubleValue.rounded() }
    if let double = value as? Double { return double == double.rounded() }
    return false
  }

  /// The object satisfies the phone's `decodeShape` for it.
  func expectShape(_ object: [String: Any], matches shape: [String: String], _ name: String) {
    for (key, kind) in shape {
      guard let value = object[key] else {
        Issue.record("\(name).\(key) is missing from the Mac's JSON")
        continue
      }
      switch kind {
      case "string": #expect(value is String, "\(name).\(key) must be a string")
      case "number": #expect(Self.isNumber(value), "\(name).\(key) must be a number")
      case "number[]":
        let array = value as? [Any]
        #expect(array?.allSatisfy(Self.isInteger) == true, "\(name).\(key) must be integers")
      default: Issue.record("unknown decodeShape kind \(kind) for \(name).\(key)")
      }
    }
  }

  static let macID = UUID(uuidString: "0F8FAD5B-D9CB-469F-A165-70867728950E")!
  static let sampleMetadata = RecordingMetadata(
    recordingID: UUID(uuidString: "6F9619FF-8B86-D011-B42D-00C04FC964FF")!,
    startedAt: Date(timeIntervalSince1970: 1_790_000_000.25), durationSeconds: 3600.25,
    byteCount: 28_800_000, sha256: Data(repeating: 1, count: 32), chunkSize: 16 * 1024 * 1024,
    format: .m4aAAC, deviceName: "iPhone")

  // MARK: - Tests

  @Test func wireDeclaresExactlyTheSixBodiesTheMacEncodes() throws {
    let declared = try Self.captures(#"export type (\w+) = \{"#, in: wire).map(\.[0])
    #expect(
      declared.sorted() == [
        "CompleteResponse", "Hello", "PairRequest", "PairResponse", "RecordingMetadata",
        "RecordingStatus",
      ])
  }

  @Test func responseBodiesCarryThePhonesFieldNamesAndKinds() throws {
    let hello = try Self.encoded(Wire.Hello(macID: Self.macID))
    #expect(hello.keys.sorted() == (try objectFields("Hello")).sorted())
    expectShape(hello, matches: try decodeShape("Hello"), "Hello")
    #expect(hello["protocol"] as? Int == Wire.protocolVersion)

    let paired = try Self.encoded(
      Wire.PairResponse(token: "t", macID: Self.macID, macName: "Studio"))
    #expect(paired.keys.sorted() == (try objectFields("PairResponse")).sorted())
    expectShape(paired, matches: try decodeShape("PairResponse"), "PairResponse")

    let status = try Self.encoded(Wire.RecordingStatus(state: .receiving, receivedChunks: [0, 2]))
    #expect(status.keys.sorted() == (try objectFields("RecordingStatus")).sorted())
    expectShape(status, matches: try decodeShape("RecordingStatus"), "RecordingStatus")
    #expect(status["state"] as? String == "receiving")

    let complete = try Self.encoded(Wire.CompleteResponse(meetingID: Self.macID))
    #expect(complete.keys.sorted() == (try objectFields("CompleteResponse")).sorted())
    expectShape(complete, matches: try decodeShape("CompleteResponse"), "CompleteResponse")
  }

  @Test func requestBodiesUseThePhonesFieldNames() throws {
    let request = try Self.encoded(Wire.PairRequest(deviceID: Self.macID, deviceName: "iPhone"))
    #expect(request.keys.sorted() == (try objectFields("PairRequest")).sorted())

    let metadata = try Self.encoded(Self.sampleMetadata)
    #expect(metadata.keys.sorted() == (try objectFields("RecordingMetadata")).sorted())
    // The value shapes `wire.test.ts` asserts for its own encoding.
    #expect(
      (metadata["recordingID"] as? String)?.wholeMatch(
        of: /[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}/) != nil)
    #expect(
      (metadata["startedAt"] as? String)?.wholeMatch(of: /\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z/)
        != nil, "ISO 8601 with three fractional digits")
    #expect(Data(base64Encoded: metadata["sha256"] as? String ?? "")?.count == 32)
    #expect(Self.isInteger(metadata["byteCount"] ?? ""))
    #expect(metadata["format"] as? String == "m4aAAC")
  }

  @Test func theMacDecodesBodiesExactlyAsThePhoneEncodesThem() throws {
    // `metadataFor` in `recording-client.ts` and `JSON.stringify`, as the
    // phone's own tests pin them: lowercase UUID, `.000Z`, standard base64.
    let phoneMetadata = """
      {"recordingID":"6f9619ff-8b86-d011-b42d-00c04fc964ff","startedAt":"2026-09-25T09:00:00.000Z",\
      "durationSeconds":61.5,"byteCount":3000,"sha256":"\(Data(repeating: 3, count: 32).base64EncodedString())",\
      "chunkSize":1024,"format":"m4aAAC","deviceName":"Nicolai's iPhone"}
      """
    let decoded = try StenoJSON.decode(RecordingMetadata.self, from: Data(phoneMetadata.utf8))
    #expect(decoded.recordingID == UUID(uuidString: "6F9619FF-8B86-D011-B42D-00C04FC964FF"))
    #expect(decoded.startedAt == Date(timeIntervalSince1970: 1_790_326_800))
    #expect(decoded.durationSeconds == 61.5)
    #expect(decoded.byteCount == 3000)
    #expect(decoded.sha256 == Data(repeating: 3, count: 32))
    #expect(decoded.chunkSize == 1024)
    #expect(decoded.format == .m4aAAC)
    #expect(decoded.deviceName == "Nicolai's iPhone")

    let phonePair = #"{"deviceID":"0f8fad5b-d9cb-469f-a165-70867728950e","deviceName":"iPhone"}"#
    let pair = try StenoJSON.decode(Wire.PairRequest.self, from: Data(phonePair.utf8))
    #expect(pair == Wire.PairRequest(deviceID: Self.macID, deviceName: "iPhone"))
  }

  @Test func enumValuesMatchCore() throws {
    #expect(try unionValues("RecordingState") == HandoverState.Kind.allCases.map(\.rawValue))
    let states = try #require(try Self.first(#"RECORDING_STATES[^=]*=\s*\[([^\]]*)\]"#, in: wire))
    #expect(
      try Self.captures(#""([^"]+)""#, in: states).map(\.[0])
        == HandoverState.Kind.allCases.map(\.rawValue))
    #expect(try Set(unionValues("AudioFormat")) == Set(AudioFormat.allCases.map(\.rawValue)))
    let recordingFormat = try #require(
      try Self.first(#"RECORDING_FORMAT: AudioFormat = "([^"]+)""#, in: recorder))
    let format = try #require(AudioFormat(rawValue: recordingFormat))
    #expect(MetadataValidation.acceptedFormats.contains(format), "the Mac accepts what it records")
  }

  @Test func constantsMatch() throws {
    #expect(try Self.first(#"export const PROTOCOL_VERSION = (\d+);"#, in: wire) == "1")
    #expect(Wire.protocolVersion == 1)
    #expect(
      try Self.first(#"export const SERVICE_TYPE = "([^"]+)";"#, in: wire) == Wire.serviceType)
    #expect(
      try Self.first(#"export const CHUNK_HASH_HEADER = "([^"]+)";"#, in: wire)
        == Wire.chunkHashHeader)
    #expect(
      try Self.first(#"authorizationHeader\(\s*scheme: ("[^)]+"),"#, in: wire)?
        .replacingOccurrences(of: "\"", with: "").replacingOccurrences(of: " ", with: "")
        == "Pairing|Bearer")
  }

  @Test func thePhonesPathsMatchTheMacsRoutes() throws {
    let id = UUID()
    func path(_ name: String) throws -> String {
      let literal = try #require(
        try Self.first(#"\b\#(name):[^`"]*[`"]([^`"]+)[`"]"#, in: wire), "no path \(name)")
      return
        literal
        .replacingOccurrences(
          of: "${encodeURIComponent(recordingID)}", with: id.uuidString.lowercased()
        )
        .replacingOccurrences(of: "${index}", with: "3")
    }
    #expect(Route.match(method: .GET, uri: try path("hello")) == .hello)
    #expect(Route.match(method: .POST, uri: try path("pair")) == .pair)
    #expect(Route.match(method: .DELETE, uri: try path("pairing")) == .unpair)
    #expect(Route.match(method: .PUT, uri: try path("recording")) == .announce(id))
    #expect(Route.match(method: .GET, uri: try path("recording")) == .status(id))
    #expect(Route.match(method: .PUT, uri: try path("chunk")) == .chunk(id, 3))
    #expect(Route.match(method: .POST, uri: try path("complete")) == .complete(id))
    #expect(
      try Self.first(#"return `(https)://"#, in: wire) == "https", "the phone speaks https only")
  }

  @Test func theQRPayloadUsesTheNamesThePhoneParses() throws {
    let payload = PairingPayload(
      macID: Self.macID, macName: "Mac", fingerprint: Data(repeating: 7, count: 32),
      secret: Data(repeating: 9, count: 32), expiresAt: Date(timeIntervalSince1970: 1_790_000_300))
    let url = try #require(URL(string: payload.urlString))
    let macQuery = Set(
      URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems?.map(\.name) ?? [])
    let phoneQuery = Set(try Self.captures(#"params\.get\("(\w+)"\)"#, in: pairing).map(\.[0]))
    #expect(macQuery == phoneQuery)
    #expect(macQuery == ["mac", "name", "fp", "secret", "exp"])

    #expect(
      try Self.first(#"const PREFIX = "([^"]+)";"#, in: pairing)
        == "\(PairingPayload.scheme)://pair/")
    #expect(try Self.first(#"version !== "([^"]+)""#, in: pairing) == PairingPayload.version)
    #expect(try Self.first(#"const FINGERPRINT_BYTES = (\d+);"#, in: pairing) == "32")
    #expect(try Self.first(#"const SECRET_BYTES = (\d+);"#, in: pairing) == "32")
    #expect(pairing.contains(#"/^\d{1,12}$/"#), "exp is at most twelve digits on both sides")
    #expect(
      url.absoluteString.hasPrefix("steno://pair/v1?mac=0f8fad5b-"), "lowercase mac in the QR")
  }
}
