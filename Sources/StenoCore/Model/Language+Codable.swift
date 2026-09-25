import Foundation

extension Locale.Language {
  /// The BCP-47 tag built from the language's own components, for example
  /// `de`, `en-US` or `zh-Hant-TW`. Unlike `minimalIdentifier` and
  /// `maximalIdentifier` it neither adds nor drops likely subtags, so a tag
  /// round-trips to an equal `Locale.Language`.
  public var stenoIdentifier: String {
    let components = explicitComponents
    return [
      components.languageCode?.identifier,
      components.script?.identifier,
      components.region?.identifier,
    ]
    .compactMap { $0 }
    .joined(separator: "-")
  }

  /// The components the language was created with. The public
  /// `languageCode`, `script` and `region` accessors and
  /// `Components(language:)` all add likely subtags (`de` reports script
  /// `Latn`), and the stored `components` property is package-internal to
  /// Foundation; its synthesized `Codable` form (`{"components": {...}}`) is
  /// the one public way to read it exactly.
  var explicitComponents: Locale.Language.Components {
    let fallback = Locale.Language.Components(
      languageCode: languageCode, script: script, region: region)
    guard let data = try? JSONEncoder().encode(self),
      let envelope = try? JSONDecoder().decode(LanguageEnvelope.self, from: data)
    else { return fallback }
    return envelope.components
  }

  /// Parses a BCP-47 tag written by `stenoIdentifier`.
  public init(stenoIdentifier: String) {
    self.init(components: Locale.Language.Components(identifier: stenoIdentifier))
  }
}

private struct LanguageEnvelope: Codable {
  var components: Locale.Language.Components
}

/// Encodes an optional `Locale.Language` as its BCP-47 tag (`"de"`, `"en-US"`)
/// instead of Foundation's component object, and omits the key when nil.
///
/// Apply it to every `Locale.Language?` property of a `Codable` type:
/// `@LanguageTag public var language: Locale.Language?`.
@propertyWrapper
public struct LanguageTag: Codable, Sendable, Equatable, Hashable {
  public var wrappedValue: Locale.Language?

  public init(wrappedValue: Locale.Language?) {
    self.wrappedValue = wrappedValue
  }

  public init(from decoder: any Decoder) throws {
    let container = try decoder.singleValueContainer()
    if container.decodeNil() {
      wrappedValue = nil
    } else {
      wrappedValue = Locale.Language(stenoIdentifier: try container.decode(String.self))
    }
  }

  public func encode(to encoder: any Encoder) throws {
    var container = encoder.singleValueContainer()
    if let wrappedValue {
      try container.encode(wrappedValue.stenoIdentifier)
    } else {
      try container.encodeNil()
    }
  }
}

extension KeyedDecodingContainer {
  /// A missing key decodes as `nil` rather than failing.
  public func decode(_ type: LanguageTag.Type, forKey key: Key) throws -> LanguageTag {
    try decodeIfPresent(LanguageTag.self, forKey: key) ?? LanguageTag(wrappedValue: nil)
  }
}

extension KeyedEncodingContainer {
  /// A nil language omits the key, matching how plain optionals encode.
  public mutating func encode(_ value: LanguageTag, forKey key: Key) throws {
    if value.wrappedValue != nil {
      try encodeIfPresent(value, forKey: key)
    }
  }
}
