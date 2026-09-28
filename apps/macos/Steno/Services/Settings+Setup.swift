import Foundation
import StenoCore
import StenoLLM

/// "Configured" is a pure function of `Settings`: the onboarding plan's
/// banner (`SetupBannerMessage`), detail rows (`SummaryStatus`,
/// `ExportStatus`), onboarding opener and page 2 read these, never a
/// mirrored class.
extension Settings {
  /// A base URL and a model are stored; the pipeline runs the LLM passes.
  var llmConfigured: Bool { LLMEndpoint(settings: self) != nil }

  /// An Obsidian vault is stored; the deliver stage has a destination.
  var vaultConfigured: Bool { obsidian != nil }
}
