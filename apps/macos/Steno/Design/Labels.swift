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

extension ProcessingProgressModel.Entry {
  /// The card, chip, list entry and menu bar row before a run's first
  /// event, while the meeting waits in the queue or the engines load.
  static let waitingTitle = "Waiting to process"
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

extension AudioRetention {
  /// What the rule does to the files, as Settings > Audio says it under the
  /// picker; onboarding reuses the days and delete sentences. Transcripts,
  /// summaries and exports are never touched by any rule.
  var footnote: String {
    switch self {
    case .keepForever:
      "Recordings stay in the folder above until you delete a meeting."
    case .keepDays(let days):
      "Each recording is deleted \(days) \(days == 1 ? "day" : "days") after it was processed and exported. Transcripts, summaries and exports are never deleted by this rule."
    case .deleteAfterProcessing:
      "Each recording is deleted as soon as it was transcribed, summarised and exported. Transcripts, summaries and exports stay."
    }
  }
}

extension ByteCountFormatter {
  /// "4.2 GB": model sizes and the recordings folder alike.
  static func fileSize(_ bytes: Int64) -> String {
    string(fromByteCount: bytes, countStyle: .file)
  }
}

extension LanguageTag {
  /// "German" for `de`, the tag itself when the locale has no name for it.
  func localizedName(in locale: Locale = .current) -> String {
    locale.localizedString(forLanguageCode: rawValue) ?? rawValue
  }
}
