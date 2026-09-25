import Foundation

/// The JSON shape of enums with payloads: a bare string for cases without a
/// payload (`"ready"`, `"keepForever"`) and a one-key object for cases with
/// one (`{"failed": "reason"}`, `{"keepDays": 30}`,
/// `{"suggested": {"personID": "…", "similarity": 0.7}}`). Readable in
/// `meeting.json` and stable, unlike the compiler-synthesized
/// `{"keepDays": {"_0": 30}}`.
///
/// Every payload enum declares its case names once, in a nested
/// `Kind: String` enum, which the wire (through these helpers) and the row
/// types (`Storage/Records.swift`) share; an unknown name throws in both.
enum CaseCoding {
  struct Key: CodingKey {
    var stringValue: String
    var intValue: Int? { nil }

    init(_ name: String) { stringValue = name }
    init?(stringValue: String) { self.stringValue = stringValue }
    init?(intValue: Int) { nil }
  }

  /// The case kind and, for a payload case, a decoder positioned on the
  /// payload. Throws `DecodingError` for a name `Kind` does not know.
  static func decode<Kind: RawRepresentable>(_ kind: Kind.Type, from decoder: any Decoder) throws
    -> (kind: Kind, payload: (any Decoder)?) where Kind.RawValue == String
  {
    let name: String
    let payload: (any Decoder)?
    if let single = try? decoder.singleValueContainer(), let bare = try? single.decode(String.self)
    {
      name = bare
      payload = nil
    } else {
      let keyed = try decoder.container(keyedBy: Key.self)
      guard keyed.allKeys.count == 1, let key = keyed.allKeys.first else {
        throw DecodingError.dataCorrupted(
          DecodingError.Context(
            codingPath: decoder.codingPath,
            debugDescription:
              "Expected a case name or a one-key object, got \(keyed.allKeys.count) keys"))
      }
      name = key.stringValue
      payload = try keyed.superDecoder(forKey: key)
    }
    guard let kind = Kind(rawValue: name) else {
      throw DecodingError.dataCorrupted(
        DecodingError.Context(
          codingPath: decoder.codingPath, debugDescription: "Unknown case \(name)"))
    }
    return (kind, payload)
  }

  static func decodePayload<T: Decodable>(
    _ type: T.Type, from payload: (any Decoder)?, case kind: some RawRepresentable<String>
  ) throws -> T {
    guard let payload else {
      throw DecodingError.dataCorrupted(
        DecodingError.Context(
          codingPath: [], debugDescription: "Case \(kind.rawValue) needs a payload"))
    }
    return try T(from: payload)
  }

  static func encode(_ kind: some RawRepresentable<String>, to encoder: any Encoder) throws {
    var container = encoder.singleValueContainer()
    try container.encode(kind.rawValue)
  }

  static func encode<P: Encodable>(
    _ kind: some RawRepresentable<String>, payload: P, to encoder: any Encoder
  ) throws {
    var container = encoder.container(keyedBy: Key.self)
    try container.encode(payload, forKey: Key(kind.rawValue))
  }
}
