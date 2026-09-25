import Foundation
import StenoCore

/// The language the summary is written in: the meeting's elected language,
/// English when the transcript was untagged. Names are English and come
/// from a fixed table first so golden prompts are machine-independent;
/// `Locale(identifier: "en_US")` answers for anything else.
public enum OutputLanguage {
  static let fallback: LanguageTag = "en"

  public static func resolve(meeting language: LanguageTag?) -> LanguageTag {
    guard let language, !language.rawValue.isEmpty else { return fallback }
    return language
  }

  /// "German" for `de` or `de-CH`, "English" for `en-US`, the tag itself
  /// when nothing knows it.
  public static func promptName(_ language: LanguageTag) -> String {
    let code = language.primarySubtag
    if let known = names[code] { return known }
    if let localized = Locale(identifier: "en_US").localizedString(forLanguageCode: code),
      !localized.isEmpty, localized != code
    {
      return localized
    }
    return language.rawValue
  }

  static let names: [String: String] = [
    "de": "German", "en": "English", "fr": "French", "es": "Spanish", "it": "Italian",
    "nl": "Dutch", "pt": "Portuguese", "pl": "Polish", "tr": "Turkish", "sv": "Swedish",
    "da": "Danish", "nb": "Norwegian", "no": "Norwegian", "fi": "Finnish", "cs": "Czech",
    "ru": "Russian", "uk": "Ukrainian", "ja": "Japanese", "zh": "Chinese", "ko": "Korean",
    "ar": "Arabic", "hi": "Hindi", "el": "Greek", "hu": "Hungarian", "ro": "Romanian",
  ]
}
