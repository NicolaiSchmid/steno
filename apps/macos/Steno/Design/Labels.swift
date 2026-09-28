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

extension RecordingEndReason {
  /// The meeting header's end-reason row. `.manual` says nothing: the user
  /// was there.
  var sentence: String? {
    switch self {
    case .manual:
      nil
    case .callEnded(let appName):
      "Ended automatically when \(appName ?? "the call app") closed the microphone."
    case .deviceLost:
      "Ended because an audio device disappeared. The recording up to that point was kept."
    case .quit:
      "Ended when Steno quit."
    case .failed:
      "Ended because the recording failed. The recording up to that point was kept."
    }
  }

  /// What the list row appends to its meta line; nil when there is nothing
  /// worth a glance.
  var listSuffix: String? {
    switch self {
    case .callEnded: "ended automatically"
    case .deviceLost: "device lost"
    case .failed: "recording failed"
    case .manual, .quit: nil
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
