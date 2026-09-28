import Foundation
import StenoCore

/// Where the pipeline is with every meeting that is `.queued` or
/// `.processing`, one entry per meeting keyed by its id. `observe` feeds it
/// from the event bus (`progress` and `deleted`) and from the meeting list;
/// the menu bar queue row, the list entry and the detail view read
/// `entry(for:)`, so the three surfaces never disagree. The fraction and
/// the estimate are core's numbers untouched; motion between events is the
/// presenter's job (`ProcessingPresentation`) and stays out of here, which
/// only stamps each entry with `now` when its event landed.
@MainActor
@Observable
final class ProcessingProgressModel {
  /// One queued or processing meeting as the model tracks it.
  struct Entry: Equatable, Sendable {
    let meetingID: UUID
    /// The last `progress` event of the run; nil before the run's first
    /// event, while the meeting waits in the queue or the engines load.
    var progress: ProcessingProgress?
    /// When `progress` landed, or when the entry was created before any
    /// event; the presenter measures its elapsed time from here.
    var since: Date

    /// The stage of the last event; read by tests, the views read `title`.
    var stage: PipelineStage? { progress?.stage }

    /// `PipelineStage.label` plus an ellipsis, "Transcribing…";
    /// `waitingTitle` before the first event. The card, the chip, the list
    /// entry and the menu bar row all read this.
    var title: String {
      guard let progress else { return Self.waitingTitle }
      return "\(progress.stage.label)…"
    }

    /// The bar's value; 0 before the first event.
    var fraction: Double { progress?.fraction ?? 0 }

    /// Wall clock the run is expected to still take; nil before the first
    /// event. Read by tests; the views word it through the presenter.
    var estimatedRemaining: Duration? { progress?.estimatedRemaining }
  }

  private(set) var entries: [UUID: Entry] = [:]
  private let now: @Sendable () -> Date

  /// `now` stamps `Entry.since`; the app passes the environment's clock so
  /// tests run on a fixed date.
  init(now: @escaping @Sendable () -> Date = Date.init) {
    self.now = now
  }

  func entry(for meetingID: UUID) -> Entry? {
    entries[meetingID]
  }

  /// Drives the model until cancelled: `apply` for every event on `events`,
  /// `meetingsChanged` for every list `meetings` delivers. The caller
  /// subscribes to the bus before the pipeline resumes its queue, so the
  /// first events of resumed runs are not missed; `AppController.launch()`
  /// and the tests wire it the same way.
  func observe(
    events: AsyncStream<MeetingEvent>, meetings: AsyncThrowingStream<[Meeting], any Error>
  ) async {
    await withTaskGroup(of: Void.self) { group in
      group.addTask { for await event in events { await self.apply(event) } }
      group.addTask {
        do {
          for try await list in meetings { await self.meetingsChanged(list) }
        } catch {
          // The list view reports store errors; nothing to do here.
        }
      }
    }
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
        entries[meetingID] = Entry(meetingID: meetingID, progress: progress, since: now())
      } else if let current = entries[meetingID] {
        var next = progress
        next.fraction = max(next.fraction, current.fraction)
        next.nextFraction = max(next.nextFraction, next.fraction)
        entries[meetingID] = Entry(meetingID: meetingID, progress: next, since: now())
      }
    case .deleted(let meetingID):
      entries[meetingID] = nil
    case .speakersNeedReview, .retentionApplied:
      break
    }
  }

  /// The meeting list as `observeMeetings()` delivers it: every meeting in
  /// `.queued` or `.processing` has an entry (without progress until its
  /// first event) and a meeting listed in another state loses its entry. A
  /// meeting absent from the list keeps its entry: a `.decode` event can
  /// land before the first list snapshot that predates the meeting's row,
  /// and the two are not ordered against each other; deletion evicts
  /// through the `deleted` event.
  func meetingsChanged(_ meetings: [Meeting]) {
    for meeting in meetings {
      if meeting.state == .queued || meeting.state == .processing {
        if entries[meeting.id] == nil {
          entries[meeting.id] = Entry(meetingID: meeting.id, since: now())
        }
      } else {
        entries[meeting.id] = nil
      }
    }
  }
}
