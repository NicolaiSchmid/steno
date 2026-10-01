import Foundation

/// The six sections of the Settings window. The page spells their titles
/// and purposes (`apps/macos/web/src/windows/settings/sections.ts`); here
/// they are the raw values of the deep-link target:
/// `AppController.openSettings(_:)` sets `requestedSettingsSection` and
/// the Settings page selects it from the `app` snapshot.
enum SettingsSection: String, CaseIterable, Identifiable, Sendable, Hashable {
  case general
  case recording
  case transcription
  case summaries
  case export
  case iphone

  var id: String { rawValue }

  var title: String {
    switch self {
    case .general: "General"
    case .recording: "Recording"
    case .transcription: "Transcription"
    case .summaries: "Summaries"
    case .export: "Export"
    case .iphone: "iPhone"
    }
  }

}
