import Foundation

/// The JSON shape of enums with payloads: a bare string for cases without a
/// payload (`"ready"`, `"keepForever"`) and a one-key object for cases with
/// one (`{"failed": "reason"}`, `{"keepDays": 30}`,
/// `{"suggested": {"personID": "…", "similarity": 0.7}}`). Readable in
/// `meeting.json` and stable, unlike the compiler-synthesized
/// `{"keepDays": {"_0": 30}}`.
enum CaseCoding {
  struct Key: CodingKey {
    var stringValue: String
    var intValue: Int? { nil }

    init(_ name: String) { stringValue = name }
    init?(stringValue: String) { self.stringValue = stringValue }
    init?(intValue: Int) { nil }
  }

  /// The case name and, for a payload case, a decoder positioned on the
  /// payload.
  static func decode(from decoder: any Decoder) throws -> (name: String, payload: (any Decoder)?) {
    if let single = try? decoder.singleValueContainer(), let name = try? single.decode(String.self)
    {
      return (name, nil)
    }
    let keyed = try decoder.container(keyedBy: Key.self)
    guard keyed.allKeys.count == 1, let key = keyed.allKeys.first else {
      throw DecodingError.dataCorrupted(
        DecodingError.Context(
          codingPath: decoder.codingPath,
          debugDescription:
            "Expected a case name or a one-key object, got \(keyed.allKeys.count) keys"))
    }
    return (key.stringValue, try keyed.superDecoder(forKey: key))
  }

  static func decodePayload<T: Decodable>(
    _ type: T.Type, from payload: (any Decoder)?, case name: String
  )
    throws -> T
  {
    guard let payload else {
      throw DecodingError.dataCorrupted(
        DecodingError.Context(codingPath: [], debugDescription: "Case \(name) needs a payload"))
    }
    return try T(from: payload)
  }

  static func unknownCase(_ name: String, in decoder: any Decoder) -> DecodingError {
    DecodingError.dataCorrupted(
      DecodingError.Context(
        codingPath: decoder.codingPath, debugDescription: "Unknown case \(name)"))
  }

  static func encode(_ name: String, to encoder: any Encoder) throws {
    var container = encoder.singleValueContainer()
    try container.encode(name)
  }

  static func encode<P: Encodable>(_ name: String, payload: P, to encoder: any Encoder) throws {
    var container = encoder.container(keyedBy: Key.self)
    try container.encode(payload, forKey: Key(name))
  }
}
