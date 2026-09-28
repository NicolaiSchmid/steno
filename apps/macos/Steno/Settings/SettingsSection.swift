import Foundation

/// The six sections of the Settings window, named by what the user gets,
/// not by the subsystem behind it. Also the deep-link target:
/// `AppController.openSettings(_:)` sets `requestedSettingsSection` and
/// `SettingsView` selects it.
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

  var systemImage: String {
    switch self {
    case .general: "gearshape"
    case .recording: "mic"
    case .transcription: "text.quote"
    case .summaries: "sparkles"
    case .export: "arrow.up.doc"
    case .iphone: "iphone"
    }
  }

  /// One sentence under the section title.
  var purpose: String {
    switch self {
    case .general: "Steno lives in the menu bar and records when you ask it to."
    case .recording: "Audio is recorded and kept on this Mac only."
    case .transcription: "Speech is turned into text on this Mac. Nothing is uploaded."
    case .summaries:
      "Summaries and tasks are written by an AI model you choose. Only the transcript text is sent to it."
    case .export: "Finished meetings can be written into an Obsidian vault as notes you own."
    case .iphone:
      "Record on your iPhone when you are away from the Mac. Recordings travel over your Wi-Fi only, encrypted to this Mac."
    }
  }
}
