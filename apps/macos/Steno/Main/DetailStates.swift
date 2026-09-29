import Foundation
import StenoCore
import SwiftUI

/// The redesign plan's "Recording and processing states" table for a tab
/// without content, as pure values: one glyph, one title and one line of
/// body per meeting state, plus the ready rows for a tab the pipeline left
/// empty. `PendingText` renders a row as an `EmptyState`; `TabText.lines`
/// reports the same words, so the snapshot test and the view cannot drift.
/// While the meeting is queued or processing and the progress model has an
/// entry, the `ProcessingCard` stands in for this row (the processing
/// progress plan's D4); the queued and processing rows here cover the
/// moment before the model has seen the meeting. The `.ready` rows for a
/// skipped summary are `SummaryStatus.skippedRow(for:)`, not this table.
struct TabPlaceholder: Hashable, Sendable {
  /// The icon well's symbol; nil for the quieter ready variant.
  var symbol: String?
  var title: String
  var body: String
  /// A small spinner under the body: the processing row.
  var showsSpinner = false
  /// A secondary "Try again" bound to `rerunSummary()`: the failed row.
  var offersTryAgain = false

  static let tryAgainTitle = "Try again"
  /// The help on a disabled "Try again" when the failure left no transcript:
  /// the re-run covers summarize and deliver only (`rerunSummary`), so it
  /// would mark the meeting ready with nothing in it.
  static let tryAgainNeedsTranscript =
    "The transcript was never produced, so there is nothing to summarise again."

  /// The row as text lines, what `TabText` reports for the tab.
  var lines: [String] { [title, body] }

  init(tab: MeetingDetailViewModel.Tab, state: MeetingState) {
    switch state {
    case .recording:
      symbol = "waveform"
      title = "Recording"
      body = "The summary, transcript and tasks appear a few minutes after you stop."
    case .queued:
      symbol = "clock"
      title = "Queued"
      body = "Processing starts when the current meeting finishes."
    case .processing:
      symbol = "waveform.badge.magnifyingglass"
      title = "Processing"
      body = "Audio stays on this Mac. This usually takes a minute or two."
      showsSpinner = true
    case .failed(let reason):
      symbol = "exclamationmark.triangle"
      title = "Processing failed"
      body = Self.firstSentence(of: reason)
      offersTryAgain = true
    case .ready:
      symbol = nil
      switch tab {
      case .summary:
        title = "No summary"
        body = "The template produced no sections."
      case .transcript:
        title = "No transcript"
        body = "No speech was recognised."
      case .tasks:
        title = "No tasks"
        body = "No tasks were found."
      case .scratchpad:
        // The scratchpad is always editable; it never shows a placeholder.
        title = ""
        body = ""
      }
    }
  }

  /// The first sentence of a failure reason: up to the first full stop that
  /// ends a sentence, or the first non-blank line. A reason without either
  /// is returned trimmed; an empty one falls back to a generic line.
  static func firstSentence(of reason: String) -> String {
    let firstLine =
      reason.split(omittingEmptySubsequences: true, whereSeparator: \.isNewline)
      .map { $0.trimmingCharacters(in: .whitespaces) }
      .first { !$0.isEmpty } ?? ""
    guard !firstLine.isEmpty else { return "The pipeline reported no reason." }
    var sentence = Substring(firstLine)
    var search = sentence.startIndex
    while let stop = sentence[search...].firstIndex(of: ".") {
      let next = sentence.index(after: stop)
      if next == sentence.endIndex || sentence[next].isWhitespace {
        sentence = sentence[..<next]
        break
      }
      search = next
    }
    return String(sentence)
  }
}

/// What the detail header shows in place of the status chip while the
/// recorder holds this meeting: the `StopButton`'s state, enabled while
/// recording and disabled (with a spinner) while the stop is finishing. Nil
/// for every meeting the recorder is not on, so a `.recording` row the
/// recorder does not yet hold (while `.starting`; never after
/// `reconcileInterruptedRecordings`) shows nothing in its place. The sidebar
/// builds the same value from `RecordingState` alone.
enum HeaderStop: Equatable, Sendable {
  case stop(since: Date)
  case stopping

  static func make(meetingID: UUID, recording: RecordingState, activeMeetingID: UUID?)
    -> HeaderStop?
  {
    guard activeMeetingID == meetingID else { return nil }
    switch recording {
    case .recording(let since): return .stop(since: since)
    case .stopping: return .stopping
    case .idle, .starting: return nil
    }
  }
}

/// A tab without content: the `TabPlaceholder` row for the meeting's state
/// as a centred `EmptyState`, id `empty-<tab>`. The failed row's "Try
/// again" runs `rerunSummary()`, which re-runs the summary over the stored
/// transcript, so it is enabled only when `canRerunSummary` holds (a
/// transcript and an endpoint) and the model is not busy; a disabled button
/// carries the reason as its help. The `.ready` rows for a skipped summary
/// are `SkippedSummaryState`; the callers pick.
struct PendingText: View {
  let tab: MeetingDetailViewModel.Tab
  let model: MeetingDetailViewModel

  var body: some View {
    if let meeting = model.meeting {
      let row = TabPlaceholder(tab: tab, state: meeting.state)
      EmptyState(
        symbol: row.symbol, title: row.title, body: row.body, showsSpinner: row.showsSpinner,
        action: row.offersTryAgain ? tryAgain : nil, id: "empty-\(tab.rawValue)")
    }
  }

  private var tryAgain: EmptyState.Action {
    EmptyState.Action(
      title: TabPlaceholder.tryAgainTitle, id: "\(tab.rawValue)-try-again",
      isEnabled: model.canRerunSummary && !model.isBusy, help: tryAgainHelp
    ) {
      Task { await model.rerunSummary() }
    }
  }

  /// Why "Try again" cannot run: no endpoint (the Actions menu's line) or no
  /// transcript. Nil when it can, or is only busy.
  private var tryAgainHelp: String? {
    if !model.llmConfigured { return SetupCopy.rerunHelp }
    if !model.hasTranscript { return TabPlaceholder.tryAgainNeedsTranscript }
    return nil
  }
}
