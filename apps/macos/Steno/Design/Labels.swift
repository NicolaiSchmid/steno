import Foundation
import StenoCore

// User-facing words for core's enums. Raw values are storage and wire
// spellings ("decode", "mic") and never reach a label.

extension MeetingSource {
  var label: String {
    switch self {
    case .macCall: "Call"
    case .macInPerson: "In person"
    case .phone: "Phone"
    }
  }
}

extension PipelineStage {
  /// What the queue row says while the stage runs.
  var label: String {
    switch self {
    case .decode: "Decoding"
    case .transcribe: "Transcribing"
    case .diarize: "Finding speakers"
    case .matchSpeakers: "Matching speakers"
    case .merge: "Merging"
    case .cleanup: "Cleaning up"
    case .summarize: "Summarising"
    case .persist: "Saving"
    case .deliver: "Delivering"
    case .retention: "Applying retention"
    }
  }
}

extension AudioLane {
  /// The lane a transcript turn came from, as the header shows it.
  var label: String {
    switch self {
    case .mic: "Mic"
    case .system: "System"
    case .mixed: "Room"
    }
  }
}

extension LanguageTag {
  /// "German" for `de`, the tag itself when the locale has no name for it.
  func localizedName(in locale: Locale = .current) -> String {
    locale.localizedString(forLanguageCode: rawValue) ?? rawValue
  }
}
