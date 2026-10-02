import Foundation
import StenoAudio
import StenoBridge
import StenoCore

// The main window's topics as pure mappings from view model state to the
// contract snapshots (plan Decision 6). One initializer per topic; each
// reads only what its topic needs, so `MainWindowBridge`'s observation
// tracking re-publishes a topic exactly when one of its inputs changes. No
// SwiftUI and no store access here: the view models stay the owners of
// every rule, these only spell their state in the wire vocabulary.

/// The helpers the mappings share.
enum MainWindowSnapshots {
  /// How many entries the page's people palette has. The index is a fold of
  /// the person's id, so one person keeps one colour across launches, in
  /// the list chips and the detail rows alike; a speaker without a person
  /// folds its own id.
  static let peoplePaletteSize = 8

  static func colorIndex(for id: UUID) -> Int {
    // FNV-1a over the 16 bytes: cheap, stable, spreads sequential ids.
    var hash: UInt32 = 2_166_136_261
    withUnsafeBytes(of: id.uuid) { bytes in
      for byte in bytes {
        hash ^= UInt32(byte)
        hash = hash &* 16_777_619
      }
    }
    return Int(hash % UInt32(peoplePaletteSize))
  }

  static func seconds(_ duration: Duration) -> Double {
    let components = duration.components
    return Double(components.seconds) + Double(components.attoseconds) / 1e18
  }

  /// `YYYY-MM-DD` of `day` in `calendar`; the page formats the label.
  static func dayString(_ day: Date, calendar: Calendar) -> String {
    let parts = calendar.dateComponents([.year, .month, .day], from: day)
    return String(format: "%04d-%02d-%02d", parts.year ?? 0, parts.month ?? 0, parts.day ?? 0)
  }

  static func source(_ source: MeetingSource) -> BridgeMeetingSource {
    switch source {
    case .macCall: .call
    case .macInPerson: .inPerson
    case .phone: .phone
    }
  }

  static func failureReason(_ state: MeetingState) -> String? {
    if case .failed(let reason) = state { return reason }
    return nil
  }

  /// `CFBundleShortVersionString`, "0" in a bundle without one (the
  /// hostless tests).
  static var bundleVersion: String {
    Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0"
  }

  /// A rendered bullet (`SummaryMarkdown.sections`: `**lead**: text`, names
  /// already substituted) back into its lead and text, the bold markers
  /// dropped: the page shows plain sentences.
  static func bullet(_ rendered: String) -> MeetingDetailSnapshot.SummarySection.Bullet {
    let afterMarker = rendered.index(rendered.startIndex, offsetBy: 2, limitedBy: rendered.endIndex)
    if rendered.hasPrefix("**"), let afterMarker,
      let close = rendered.range(of: "**: ", range: afterMarker..<rendered.endIndex)
    {
      return .init(
        lead: String(rendered[afterMarker..<close.lowerBound]),
        text: plain(String(rendered[close.upperBound...])))
    }
    return .init(lead: "", text: plain(rendered))
  }

  static func plain(_ inline: String) -> String {
    inline.replacingOccurrences(of: "**", with: "")
  }

  /// Segments grouped into speaker turns: consecutive segments of one
  /// speaker become one turn spanning them; a segment without a speaker is
  /// always its own turn. `displayName` names a speaker id (nil reads as
  /// "Unknown" through `MeetingDetailViewModel.displayName(forSpeaker:)`).
  static func turns(
    _ segments: [TranscriptSegment], displayName: (UUID?) -> String
  ) -> [MeetingDetailSnapshot.Turn] {
    var turns: [MeetingDetailSnapshot.Turn] = []
    for segment in segments.sorted(by: { $0.start < $1.start }) {
      if let index = turns.indices.last, let speaker = segment.speakerID,
        turns[index].speakerID == speaker
      {
        turns[index].text += " " + segment.text
        turns[index].endSeconds = max(turns[index].endSeconds, segment.end)
      } else {
        turns.append(
          MeetingDetailSnapshot.Turn(
            id: segment.id, speakerID: segment.speakerID,
            speakerName: displayName(segment.speakerID), startSeconds: segment.start,
            endSeconds: segment.end, text: segment.text))
      }
    }
    return turns
  }

  /// One delivery as the footer words it: the destination, then "Exported
  /// 10:02", "Pending" or "Failed: reason".
  static func deliveryLine(_ delivery: Delivery) -> String {
    let status: String
    switch delivery.status {
    case .pending:
      status = "Pending"
    case .delivered:
      if let at = delivery.lastAttemptAt {
        status = "Exported \(DisplayFormat.time(at))"
      } else {
        status = "Exported"
      }
    case .failed(let message):
      status = "Failed: \(message)"
    }
    return "\(delivery.destinationDisplayName) · \(status)"
  }
}

// MARK: - Vocabulary shared with the commands

extension MeetingDetailSnapshot.Tab {
  /// The model's `scratchpad` is the page's `notes`; the other three match.
  init(_ tab: MeetingDetailViewModel.Tab) {
    switch tab {
    case .summary: self = .summary
    case .transcript: self = .transcript
    case .tasks: self = .tasks
    case .scratchpad: self = .notes
    }
  }

  var modelTab: MeetingDetailViewModel.Tab {
    switch self {
    case .summary: .summary
    case .transcript: .transcript
    case .tasks: .tasks
    case .notes: .scratchpad
    }
  }
}

extension BridgeListFilter {
  /// The two enums share their raw values (a case one gains without the
  /// other is a crash in tests, not a silent `.all`).
  init(_ filter: MeetingListViewModel.StateFilter) {
    self.init(rawValue: filter.rawValue)!
  }

  var modelFilter: MeetingListViewModel.StateFilter {
    MeetingListViewModel.StateFilter(rawValue: rawValue)!
  }
}

// MARK: - app

extension AppSnapshot {
  /// The setup banner shows while at least one meeting exists (`hasMeetings`
  /// is the bridge's flag, flipped when the list empties or fills, so this
  /// topic does not follow every list change), the configuration is
  /// incomplete and "Not now" was not pressed this launch. `phone` stays nil: the
  /// handover service is an actor and its paired devices are an async
  /// query, which a synchronous snapshot cannot make; the iPhone card comes
  /// with the Settings bridge. Deep links are the controller's pending
  /// requests as they stand; the bridge consumes the meeting request.
  @MainActor
  init(
    controller: AppController, hasMeetings: Bool = true,
    version: String = MainWindowSnapshots.bundleVersion
  ) {
    var banner: SetupBanner?
    if hasMeetings, !controller.setupBannerDismissed, let message = controller.setupBannerMessage {
      banner = SetupBanner(
        title: message.title, body: message.body, offersSummaries: message.offersSummaries,
        offersVault: message.offersVault)
    }
    self.init(
      version: version, setupBanner: banner, phone: nil,
      requestedMeetingID: controller.requestedMeetingID,
      requestedSettingsSection: controller.requestedSettingsSection.flatMap {
        BridgeSettingsSection(rawValue: $0.rawValue)
      })
  }
}

// MARK: - recording

extension RecordingSnapshot {
  /// The recorder as the sidebar control rendered it: the state, the start
  /// for the elapsed time, the lanes' levels while recording only (the
  /// recorder keeps the last reading through `.stopping`, and a frozen
  /// meter under a spinner reads as live), the armed auto-stop as seconds
  /// so the page counts down, and the messages.
  @MainActor
  init(recorder: RecordingController) {
    let state: RecordingSnapshot.State
    var startedAt: Date?
    switch recorder.recording {
    case .idle:
      state = .idle
    case .starting:
      state = .starting
    case .recording(let since):
      state = .recording
      startedAt = since
    case .stopping:
      state = .stopping
    }
    let mode: BridgeCaptureMode? = recorder.activeMode.map { $0 == .call ? .call : .inPerson }
    var level: RecordingSnapshot.Level?
    if state == .recording, let levels = recorder.levels {
      level = RecordingSnapshot.Level(
        mic: Double(LevelBars.fraction(levels.mic.rms)),
        system: levels.system.map { Double(LevelBars.fraction($0.rms)) } ?? 0)
    }
    let autoStop = recorder.autoStop.map { armed in
      RecordingSnapshot.AutoStop(
        remainingSeconds: MainWindowSnapshots.seconds(armed.countdown.remaining),
        totalSeconds: MainWindowSnapshots.seconds(armed.countdown.duration),
        reason: "\(armed.appName ?? "The call app") closed the microphone.")
    }
    self.init(
      state: state, startedAt: startedAt, mode: mode, callApp: recorder.activeCallApp,
      meetingID: recorder.activeMeetingID, level: level, autoStop: autoStop,
      deniedPermissions: recorder.deniedPermissions.compactMap {
        BridgePermissionKind(rawValue: $0.rawValue)
      },
      warning: recorder.lastWarning, error: recorder.lastError)
  }
}

// MARK: - progress

extension ProgressSnapshot {
  /// Every queued or processing meeting the model tracks, oldest entry
  /// first. The fraction and the estimate are core's numbers untouched; the
  /// page moves the bar between events as `ProcessingPresentation` did.
  @MainActor
  init(model: ProcessingProgressModel) {
    let entries = model.entries.values
      .sorted { ($0.since, $0.meetingID.uuidString) < ($1.since, $1.meetingID.uuidString) }
      .map { entry in
        ProgressSnapshot.Entry(
          meetingID: entry.meetingID, stage: entry.stage?.rawValue ?? "waiting",
          title: entry.title, fraction: entry.fraction,
          estimatedRemainingSeconds: entry.estimatedRemaining.map(MainWindowSnapshots.seconds))
      }
    self.init(entries: entries)
  }
}

// MARK: - meetings.list

extension MeetingsListSnapshot {
  /// The list column: the filters as set, the nav counts (every meeting,
  /// before the tag filter and the query), the tags with how many meetings
  /// carry each, and the day groups in the model's calendar. Reads the list
  /// model alone: a queued or processing row's stage comes from the
  /// `progress` topic on the page, so a progress tick never re-publishes
  /// the list.
  @MainActor
  init(list: MeetingListViewModel) {
    var tagCounts: [String: Int] = [:]
    for meeting in list.all {
      for tag in meeting.tags { tagCounts[tag, default: 0] += 1 }
    }
    let groups = list.dayGroups.map { group in
      DayGroup(
        day: MainWindowSnapshots.dayString(group.day, calendar: list.calendar),
        meetings: group.meetings.map { MeetingRow(meeting: $0, list: list) })
    }
    self.init(
      filter: BridgeListFilter(list.stateFilter), tagFilter: list.tagFilter, query: list.query,
      counts: Counts(
        all: list.count(for: .all), processing: list.count(for: .processing),
        ready: list.count(for: .ready), failed: list.count(for: .failed)),
      tags: tagCounts.keys.sorted().map { Tag(name: $0, count: tagCounts[$0] ?? 0) },
      groups: groups, selection: list.selection, error: list.error)
  }
}

extension MeetingRow {
  /// One entry: the display title (derived for the intake's default), the
  /// preview as the first summary bullet (nil while the meeting is queued or
  /// processing: the page shows the stage from the `progress` topic), the
  /// speaker chips from the list's speaker map.
  @MainActor
  init(meeting: Meeting, list: MeetingListViewModel) {
    let bullets = meeting.summary?.sections.flatMap(\.bullets) ?? []
    let preview: String?
    switch meeting.state {
    case .queued, .processing:
      preview = nil
    case .recording, .ready, .failed:
      preview = bullets.first.map { $0.lead.isEmpty ? $0.text : "\($0.lead): \($0.text)" }
    }
    let speakers = (list.speakersByMeeting[meeting.id] ?? []).map { speaker in
      SpeakerChip(
        speaker: speaker, person: speaker.assignment.personID.flatMap { list.personsByID[$0] })
    }
    self.init(
      id: meeting.id, title: meeting.displayTitle(calendar: list.calendar),
      startedAt: meeting.startedAt, durationSeconds: meeting.duration,
      source: MainWindowSnapshots.source(meeting.source), state: meeting.state.kind,
      failureReason: MainWindowSnapshots.failureReason(meeting.state), preview: preview,
      hasSummary: !bullets.isEmpty, speakers: speakers, tags: meeting.tags)
  }
}

extension SpeakerChip {
  /// The person's first letter, or "?" for a speaker nobody has named yet.
  init(speaker: Speaker, person: Person?) {
    let name = person?.displayName.trimmingCharacters(in: .whitespaces) ?? ""
    self.init(
      id: speaker.id, initial: name.first.map { String($0).uppercased() } ?? "?",
      colorIndex: MainWindowSnapshots.colorIndex(for: person?.id ?? speaker.id),
      isConfirmed: speaker.assignment.isConfirmed)
  }
}

// MARK: - meeting.detail

extension MeetingDetailSnapshot {
  /// The detail pane once its export has loaded; nil before (the bridge
  /// publishes `null` for the topic then). Every derived value comes from
  /// the view model's own accessors, so the rules the view model tests pin
  /// (retention, summary status, export status, re-run guards) are the ones
  /// the page renders.
  @MainActor
  init?(detail: MeetingDetailViewModel) {
    guard let export = detail.export else { return nil }
    let meeting = export.meeting
    let speakers = detail.speakers
    let playing = speakers.playing
    let speakerRows = speakers.rows.map { row in
      MeetingDetailSnapshot.Speaker(
        id: row.id, clusterLabel: row.speaker.clusterLabel, displayName: row.displayName,
        assignment: row.speaker.assignment.kind, personID: row.speaker.assignment.personID,
        email: row.person?.email, suggestionName: speakers.prefill(for: row.id),
        colorIndex: MainWindowSnapshots.colorIndex(for: row.person?.id ?? row.id),
        hasClip: row.canPlay, isPlaying: playing == row.id)
    }

    let retentionKind: Retention.Kind
    var deletesAt: Date?
    switch detail.recordingStatus {
    case nil: retentionKind = .keptForever
    case .deleted?: retentionKind = .deleted
    case .deletes(let on)?:
      retentionKind = .deletesOn
      deletesAt = on
    case .keptUntilExportSucceeds?: retentionKind = .keptUntilExportSucceeds
    case .keptProcessingFailed?: retentionKind = .keptProcessingFailed
    case .keptWhileProcessing?: retentionKind = .keptWhileProcessing
    }

    let statusKind: MeetingDetailSnapshot.SummaryStatus.Kind
    switch detail.summaryStatus {
    case .pending: statusKind = .pending
    case .present: statusKind = .present
    case .skippedUnconfigured: statusKind = .skippedUnconfigured
    case .skippedRunnable: statusKind = .skippedRunnable
    }
    let skipped = detail.summaryStatus.skippedRow(for: .summary)

    let tasks = export.tasks.map { task in
      let assignee = task.assigneePersonID.flatMap { export.person(id: $0) }
      return MeetingDetailSnapshot.Task(
        id: task.id, text: task.text, assigneeName: assignee?.displayName ?? task.assigneeName,
        assigneeColorIndex: assignee.map { MainWindowSnapshots.colorIndex(for: $0.id) },
        dueDate: task.dueDate, priority: task.priority, done: task.done)
    }

    let exportSnapshot: Export
    switch detail.exportStatus {
    case .noVault:
      exportSnapshot = Export(
        status: .notConfigured, message: SetupCopy.notExportedNoVault, canReexport: false,
        canReveal: false)
    case .notExported:
      exportSnapshot = Export(
        status: .pending, message: SetupCopy.notExportedYet, canReexport: detail.canReexport,
        canReveal: false)
    case .exported(let deliveries):
      let anyFailed = deliveries.contains { $0.status.kind == .failed }
      let anyPending = deliveries.contains { $0.status == .pending }
      exportSnapshot = Export(
        status: anyFailed ? .failed : anyPending ? .pending : .delivered,
        message: deliveries.map(MainWindowSnapshots.deliveryLine).joined(separator: "; "),
        canReexport: detail.canReexport, canReveal: deliveries.contains { $0.receipt != nil })
    }

    self.init(
      id: meeting.id, title: meeting.displayTitle(), startedAt: meeting.startedAt,
      durationSeconds: meeting.duration, language: meeting.language?.rawValue,
      source: MainWindowSnapshots.source(meeting.source), state: meeting.state.kind,
      failureReason: MainWindowSnapshots.failureReason(meeting.state),
      endReason: meeting.endReason?.sentence, tags: meeting.tags, tab: Tab(detail.tab),
      retention: Retention(
        kind: retentionKind, deletesAt: deletesAt, keepsAudio: detail.keepsAudio,
        showsKeepToggle: detail.showsKeepToggle, filesExist: detail.recordingFilesExist),
      speakers: speakerRows,
      templates: detail.templates.map { Template(id: $0.id, name: $0.displayName) },
      templateID: meeting.templateID,
      summaryStatus: MeetingDetailSnapshot.SummaryStatus(
        kind: statusKind, title: skipped?.title, body: skipped?.body,
        actionTitle: skipped?.action.title),
      summary: detail.summarySections.map { section in
        SummarySection(
          id: section.id, heading: section.heading,
          bullets: section.bullets.map(MainWindowSnapshots.bullet))
      },
      transcript: MainWindowSnapshots.turns(export.segments) { detail.displayName(forSpeaker: $0) },
      tasks: tasks, decisions: export.decisions.map(\.text), notes: meeting.scratchpad,
      export: exportSnapshot, canRerunSummary: detail.canRerunSummary, isBusy: detail.isBusy,
      error: detail.error)
  }
}
