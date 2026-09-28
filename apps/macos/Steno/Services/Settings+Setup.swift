import Foundation
import StenoCore
import StenoLLM

/// The model type by an unambiguous name for files that also import
/// SwiftUI, whose `Settings` scene shadows it (and `StenoCore.Settings`
/// resolves to the module's namespace enum, not the module).
typealias StenoSettings = Settings

/// "Configured" is a pure function of `Settings`: the onboarding plan's
/// banner, detail rows and onboarding page read these, never a mirrored
/// class. The view models already observe `Settings`, so nothing else has
/// to.
extension Settings {
  /// A base URL and a model are stored; the pipeline runs the LLM passes.
  var llmConfigured: Bool { LLMEndpoint(settings: self) != nil }

  /// An Obsidian vault is stored; the deliver stage has a destination.
  var vaultConfigured: Bool { obsidian != nil }
}

/// The Settings tabs, in the order the scene shows them. `AppController.
/// openSettings(_:)` requests one; `SettingsView` selects it.
enum SettingsTab: String, CaseIterable, Sendable {
  case general
  case audio
  case speech
  case llm
  case obsidian
  case phones
  case updates
}

/// What the main window's setup banner says, derived from `Settings`; nil
/// when both the endpoint and the vault are configured. Copy comes from the
/// onboarding plan's "Main window banner" section.
enum SetupBannerMessage: Equatable, Sendable {
  case bothMissing
  case endpointMissing
  case vaultMissing

  init?(settings: Settings) {
    switch (settings.llmConfigured, settings.vaultConfigured) {
    case (true, true): return nil
    case (false, false): self = .bothMissing
    case (false, true): self = .endpointMissing
    case (true, false): self = .vaultMissing
    }
  }

  var text: String {
    switch self {
    case .bothMissing:
      "Summaries and export are off. Steno has no LLM endpoint and no Obsidian vault yet, so meetings keep a raw transcript on this Mac."
    case .endpointMissing:
      "Summaries are off. Steno has no LLM endpoint yet, so meetings keep a raw transcript."
    case .vaultMissing:
      "Export is off. Steno has no Obsidian vault yet, so meetings stay on this Mac."
    }
  }

  /// The "Set up summaries" button (Settings > LLM).
  var offersSummaries: Bool { self != .vaultMissing }

  /// The "Choose a vault" button (Settings > Obsidian).
  var offersVault: Bool { self != .endpointMissing }
}
