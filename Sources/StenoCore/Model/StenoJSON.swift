import Foundation

/// The one JSON convention: camelCase keys, sorted keys, pretty printed, ISO
/// 8601 dates with fractional seconds, `Data` as standard base64. Used for
/// `meeting.json`, JSON columns and the handover wire, so two encodes of equal
/// values are byte-identical.
public enum StenoJSON {
  public static func encoder() -> JSONEncoder {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys, .prettyPrinted, .withoutEscapingSlashes]
    encoder.dateEncodingStrategy = .custom { date, encoder in
      var container = encoder.singleValueContainer()
      try container.encode(format(date))
    }
    encoder.dataEncodingStrategy = .base64
    return encoder
  }

  public static func decoder() -> JSONDecoder {
    let decoder = JSONDecoder()
    decoder.dateDecodingStrategy = .custom { decoder in
      let container = try decoder.singleValueContainer()
      let string = try container.decode(String.self)
      guard let date = parse(string) else {
        throw DecodingError.dataCorruptedError(
          in: container, debugDescription: "Not an ISO 8601 date: \(string)")
      }
      return date
    }
    decoder.dataDecodingStrategy = .base64
    return decoder
  }

  public static func encode<T: Encodable>(_ value: T) throws -> Data {
    try encoder().encode(value)
  }

  public static func decode<T: Decodable>(_ type: T.Type, from data: Data) throws -> T {
    try decoder().decode(type, from: data)
  }

  private static let fractional = Date.ISO8601FormatStyle(includingFractionalSeconds: true)
  private static let whole = Date.ISO8601FormatStyle(includingFractionalSeconds: false)

  /// `2026-09-25T10:00:00.000Z`, always UTC, always three fraction digits.
  public static func format(_ date: Date) -> String {
    fractional.format(date)
  }

  /// Accepts fractional and whole-second forms.
  public static func parse(_ string: String) -> Date? {
    if let date = try? fractional.parse(string) { return date }
    return try? whole.parse(string)
  }
}
