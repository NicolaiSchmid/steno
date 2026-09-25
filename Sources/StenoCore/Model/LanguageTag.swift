import Foundation

/// A BCP-47 language tag (`de`, `en-US`, `zh-Hant-TW`): the model's language
/// value, stored and encoded as its string. `Locale.Language` appears only at
/// the `SpeechEngine` boundary (`supportedLanguages`, the `hint`) through
/// `language` and `init(_:)`.
public struct LanguageTag: RawRepresentable, Codable, Hashable, Sendable, CustomStringConvertible {
  public var rawValue: String

  public init(rawValue: String) {
    self.rawValue = rawValue
  }

  /// The tag built from the language's own components, for example `de`,
  /// `en-US` or `zh-Hant-TW`. Unlike `minimalIdentifier` and
  /// `maximalIdentifier` it neither adds nor drops likely subtags, so the tag
  /// round-trips to an equal `Locale.Language`.
  public init(_ language: Locale.Language) {
    let components = language.explicitComponents
    rawValue = [
      components.languageCode?.identifier,
      components.script?.identifier,
      components.region?.identifier,
    ]
    .compactMap { $0 }
    .joined(separator: "-")
  }

  public var language: Locale.Language {
    Locale.Language(components: Locale.Language.Components(identifier: rawValue))
  }

  public var description: String { rawValue }
}

extension LanguageTag: ExpressibleByStringLiteral {
  public init(stringLiteral value: String) {
    self.init(rawValue: value)
  }
}

extension Locale.Language {
  /// The components the language was created with. The public
  /// `languageCode`, `script` and `region` accessors and
  /// `Components(language:)` all add likely subtags (`de` reports script
  /// `Latn`), and the stored `components` property is package-internal to
  /// Foundation; its synthesized `Codable` form (`{"components": {...}}`) is
  /// the one public way to read it exactly. Used once per tag construction,
  /// at the engine boundary.
  fileprivate var explicitComponents: Locale.Language.Components {
    let fallback = Locale.Language.Components(
      languageCode: languageCode, script: script, region: region)
    guard let data = try? JSONEncoder().encode(self),
      let envelope = try? JSONDecoder().decode(LanguageEnvelope.self, from: data)
    else { return fallback }
    return envelope.components
  }
}

private struct LanguageEnvelope: Codable {
  var components: Locale.Language.Components
}
