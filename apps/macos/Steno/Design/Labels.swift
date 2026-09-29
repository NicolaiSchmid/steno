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

extension MeetingState {
  /// The state word: the list entry's accessibility value, the preview line
  /// while there is nothing better to say.
  var label: String {
    switch self {
    case .recording: "Recording"
    case .queued: "Queued"
    case .processing: "Processing"
    case .ready: "Ready"
    case .failed: "Failed"
    }
  }
}

extension MeetingListViewModel.StateFilter {
  /// The nav row's glyph.
  var symbolName: String {
    switch self {
    case .all: "rectangle.stack"
    case .processing: "clock"
    case .ready: "checkmark.circle"
    case .failed: "exclamationmark.triangle"
    }
  }
}

/// Every wall-clock string the list and the header render, through
/// `Date.FormatStyle` so 12 versus 24 hour, "Sep 28" versus "28. Sept." and
/// weekday names follow the locale; never a literal pattern. The calendar
/// carries the time zone, so a test can view a Berlin recording from UTC.
enum DisplayFormat {
  private static func style(calendar: Calendar, locale: Locale) -> Date.FormatStyle {
    Date.FormatStyle(locale: locale, calendar: calendar, timeZone: calendar.timeZone)
  }

  /// "10:06" or "10:06 AM".
  static func time(_ date: Date, calendar: Calendar = .current, locale: Locale = .current)
    -> String
  {
    date.formatted(
      style(calendar: calendar, locale: locale).hour(.defaultDigits(amPM: .abbreviated)).minute())
  }

  /// "Monday".
  static func weekday(_ date: Date, calendar: Calendar = .current, locale: Locale = .current)
    -> String
  {
    date.formatted(style(calendar: calendar, locale: locale).weekday(.wide))
  }

  /// "Sep 28".
  static func monthDay(_ date: Date, calendar: Calendar = .current, locale: Locale = .current)
    -> String
  {
    date.formatted(style(calendar: calendar, locale: locale).month(.abbreviated).day())
  }
}

extension Meeting {
  /// The last six days read as a weekday in the derived title; older days
  /// as month and day.
  static let weekdayTitleDays = 6

  /// The stored `title` came from nobody: the intake's machine default. The
  /// screen derives a title from `startedAt` instead and shows the source
  /// as a chip beside it.
  var isTitleDerived: Bool { titleOrigin == .default }

  /// What the list entry and the detail heading call the meeting: the
  /// stored title unless it is the intake's default, in which case "Monday
  /// 10:06" for the last six days and "Sep 28 10:06" before that, in the
  /// viewer's calendar, time zone and locale. The stored value is never
  /// touched; exports, search and the Obsidian slug keep it.
  func displayTitle(now: Date = Date(), calendar: Calendar = .current, locale: Locale = .current)
    -> String
  {
    guard isTitleDerived else { return title }
    let days =
      calendar.dateComponents(
        [.day], from: calendar.startOfDay(for: startedAt), to: calendar.startOfDay(for: now)
      ).day ?? Int.max
    let day =
      (0...Self.weekdayTitleDays).contains(days)
      ? DisplayFormat.weekday(startedAt, calendar: calendar, locale: locale)
      : DisplayFormat.monthDay(startedAt, calendar: calendar, locale: locale)
    return "\(day) \(DisplayFormat.time(startedAt, calendar: calendar, locale: locale))"
  }

  /// The one-line preview under the entry's title: the first bullet of the
  /// summary ("lead: text"), else what the state says. Pure: while the
  /// pipeline runs, the view prefers the progress model's stage title.
  var previewLine: String {
    switch state {
    case .recording: MeetingState.recording.label
    case .queued: ProcessingProgressModel.Entry.waitingTitle
    case .processing: MeetingState.processing.label
    case .failed(let reason): reason.firstLine ?? state.label
    case .ready: summary?.plainText.firstLine ?? "No summary"
    }
  }
}

extension String {
  /// The first non-blank line, trimmed; nil when there is none. Only
  /// `previewLine` reads it.
  fileprivate var firstLine: String? {
    for line in split(omittingEmptySubsequences: true, whereSeparator: \.isNewline) {
      let trimmed = line.trimmingCharacters(in: .whitespaces)
      if !trimmed.isEmpty { return trimmed }
    }
    return nil
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
