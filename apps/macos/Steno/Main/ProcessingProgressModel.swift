import Foundation
import StenoCore

/// Where the pipeline is with every meeting that is `.queued` or
/// `.processing`, one entry per meeting keyed by its id. `AppController`
/// feeds it from the event bus (`progress` and `deleted`) and from the
/// meeting list; the menu bar queue row, the list entry and the detail
/// view read `entry(for:)`, so the three surfaces never disagree. The
/// fraction and the estimate are core's numbers untouched; motion between
/// events is the presenter's job and stays out of here.
@MainActor
@Observable
final class ProcessingProgressModel {
  struct Entry: Equatable, Sendable {
    let meetingID: UUID
    /// The last `progress` event of the run; nil before the run's first
    /// event, while the meeting waits in the queue or the engines load.
    var progress: ProcessingProgress?

    init(meetingID: UUID, progress: ProcessingProgress? = nil) {
      self.meetingID = meetingID
      self.progress = progress
    }

    var stage: PipelineStage? { progress?.stage }

    /// `PipelineStage.label` plus an ellipsis, "Transcribing…"; "Waiting to
    /// process" before the first event. The card, the chip and the menu bar
    /// row all read this.
    var title: String {
      guard let progress else { return "Waiting to process" }
      return "\(progress.stage.label)…"
    }

    /// The bar's value; 0 before the first event.
    var fraction: Double { progress?.fraction ?? 0 }

    /// Wall clock the run is expected to still take; nil before the first
    /// event.
    var estimatedRemaining: Duration? { progress?.estimatedRemaining }
  }

  private(set) var entries: [UUID: Entry] = [:]

  func entry(for meetingID: UUID) -> Entry? {
    entries[meetingID]
  }

  /// `progress` at `.decode`, the first event of a run, starts a new entry
  /// even when an earlier run's entry is still there; any other stage
  /// updates the meeting's entry and never lowers its fraction. Progress for
  /// a meeting without an entry (a summary re-run on a `.ready` meeting)
  /// drives nothing. `deleted` evicts.
  func apply(_ event: MeetingEvent) {
    switch event {
    case .progress(let meetingID, let progress):
      if progress.stage == .decode {
        entries[meetingID] = Entry(meetingID: meetingID, progress: progress)
      } else if let current = entries[meetingID] {
        var next = progress
        next.fraction = max(next.fraction, current.fraction)
        next.nextFraction = max(next.nextFraction, next.fraction)
        entries[meetingID] = Entry(meetingID: meetingID, progress: next)
      }
    case .deleted(let meetingID):
      entries[meetingID] = nil
    case .speakersNeedReview, .retentionApplied:
      break
    }
  }

  /// The meeting list as `observeMeetings()` delivers it: every meeting in
  /// `.queued` or `.processing` has an entry (without progress until its
  /// first event) and a meeting that left those states loses its entry.
  func meetingsChanged(_ meetings: [Meeting]) {
    let live = Set(
      meetings.filter { $0.state == .queued || $0.state == .processing }.map(\.id))
    for id in live where entries[id] == nil {
      entries[id] = Entry(meetingID: id)
    }
    for id in Array(entries.keys) where !live.contains(id) {
      entries[id] = nil
    }
  }
}
