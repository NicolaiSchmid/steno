import Foundation
import StenoCore

/// The engines this module can build. `parakeetV3` and
/// `whisperKitLargeV3Turbo` are user-selectable; `parakeetUltra` and
/// `parakeetDE` are bake-off entrants until the result plan promotes one.
public enum SpeechEngineID: String, Sendable, CaseIterable, Codable, Hashable {
  case parakeetV3 = "parakeet-v3"
  case parakeetUltra = "parakeet-ultra"
  case parakeetDE = "parakeet-de"
  case whisperKitLargeV3Turbo = "whisperkit-large-v3-turbo"

  /// The engines the settings pane offers.
  public static let userSelectable: [SpeechEngineID] = [.parakeetV3, .whisperKitLargeV3Turbo]

  /// The model the engine loads; the diarizer is separate.
  public var asset: ModelAsset {
    switch self {
    case .parakeetV3: .parakeetV3
    case .parakeetUltra: .parakeetUltra
    case .parakeetDE: .parakeetDE
    case .whisperKitLargeV3Turbo: .whisperLargeV3Turbo
    }
  }

  /// Languages as tags; engines expose them as `Locale.Language` through
  /// `supportedLanguages`.
  public var supportedLanguageTags: Set<LanguageTag> {
    switch self {
    case .parakeetV3, .parakeetUltra: Self.parakeetV3Languages
    case .parakeetDE: ["de"]
    case .whisperKitLargeV3Turbo: Self.whisperLanguages
    }
  }

  public var supportedLanguages: Set<Locale.Language> {
    Set(supportedLanguageTags.map(\.language))
  }

  /// The 25 European languages of Parakeet TDT v3 (NVIDIA model card).
  private static let parakeetV3Languages: Set<LanguageTag> = [
    "bg", "hr", "cs", "da", "nl", "en", "et", "fi", "fr", "de", "el", "hu", "it", "lv", "lt", "mt",
    "pl", "pt", "ro", "sk", "sl", "es", "sv", "ru", "uk",
  ]

  /// Whisper's 99 languages by ISO 639-1 code (or the code Whisper uses where
  /// none exists), from the OpenAI tokenizer table.
  private static let whisperLanguages: Set<LanguageTag> = Set(
    """
    en zh de es ru ko fr ja pt tr pl ca nl ar sv it id hi fi vi he uk el ms cs ro da hu ta no th \
    ur hr bg lt la mi ml cy sk te fa lv bn sr az sl kn et mk br eu is hy ne mn bs kk sq sw gl mr \
    pa si km sn yo so af oc ka be tg sd gu am yi lo uz fo ht ps tk nn mt sa lb my bo tl mg as tt \
    haw ln ha ba jw su yue
    """
    .split(separator: " ").map { LanguageTag(rawValue: String($0)) })
}

/// Builds the engine for an id over one `ModelStore`. The app injects this
/// as a closure so it never imports the frameworks itself.
public func makeSpeechEngine(_ id: SpeechEngineID, models: ModelStore) throws -> any SpeechEngine {
  #if canImport(FluidAudio) && canImport(WhisperKit)
    switch id {
    case .parakeetV3, .parakeetUltra, .parakeetDE: return ParakeetEngine(id: id, models: models)
    case .whisperKitLargeV3Turbo: return WhisperKitEngine(models: models)
    }
  #else
    throw SpeechEngineError.unavailable(id)
  #endif
}

/// The FluidAudio offline diarizer over one `ModelStore`; throws where the
/// framework is missing so callers without it (Linux) fail at wiring time.
public func makeDiarizer(models: ModelStore, config: FluidDiarizerConfig = .default) throws
  -> any Diarizer
{
  #if canImport(FluidAudio)
    return FluidDiarizer(models: models, config: config)
  #else
    throw SpeechEngineError.unavailable(.parakeetV3)
  #endif
}

/// `makeSpeechEngine` for the string stored in `Settings.speechEngineID`.
public func makeSpeechEngine(id rawID: String, models: ModelStore) throws -> any SpeechEngine {
  guard let id = SpeechEngineID(rawValue: rawID) else {
    throw SpeechEngineError.unknownEngine(rawID)
  }
  return try makeSpeechEngine(id, models: models)
}
