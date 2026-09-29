import Foundation
import StenoCore
import StenoLLM

/// "Configured" is a pure function of `Settings`: the onboarding plan's
/// banner (`SetupBannerMessage`), detail rows (`SummaryStatus`,
/// `ExportStatus`), onboarding opener and page 2 read these, never a
/// mirrored class.
extension Settings {
  /// The chosen provider is set up (endpoint: URL and model stored; Codex:
  /// confirmed and a model picked); the pipeline runs the LLM passes.
  var llmConfigured: Bool { LLMEndpoint(settings: self) != nil }

  /// An Obsidian vault is stored; the deliver stage has a destination.
  var vaultConfigured: Bool { obsidian != nil }
}
