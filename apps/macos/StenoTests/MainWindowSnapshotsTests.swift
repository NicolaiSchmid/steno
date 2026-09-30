import Foundation
import StenoBridge
import StenoCore
import Testing

// The main window's snapshots over the preview store, and the bridge's
// command routing, hostless: no web view anywhere, a recording sink where
// `WebBridge` would be. The view models' rules stay pinned by their own
// tests; here the wire shape the page renders, and that a command reaches
// its view model and comes back as a publish.

@MainActor
private final class RecordingSink: BridgeEventSink {
  private(set) var events: [BridgeEvent] = []

  func emit(_ event: BridgeEvent) {
    events.append(event)
  }

  func last(_ topic: BridgeTopic) -> JSONValue? {
    events.last { $0.topic == topic }?.payload
  }
}

/// Polls `condition` every 10 ms up to `timeout` and records an issue on
/// timeout. Used only where a store observation or a main-actor hop must be
/// given time to deliver.
@MainActor
private func eventually(
  _ description: String, timeout: Duration = .seconds(10), _ condition: @MainActor () -> Bool
) async {
  let clock = ContinuousClock()
  let deadline = clock.now + timeout
  while !condition() {
    if clock.now >= deadline {
      Issue.record("timed out waiting for \(description)")
      return
    }
    try? await Task.sleep(for: .milliseconds(10))
  }
}

/// The day boundaries the list tests count on, wherever the runner is.
private var utc: Calendar {
  var calendar = Calendar(identifier: .gregorian)
  calendar.timeZone = TimeZone(identifier: "UTC")!
  return calendar
}

private func request(_ id: String, _ method: String, _ params: Any = NSNull()) -> [String: Any] {
  ["id": id, "method": method, "params": params]
}

// MARK: - Pure helpers

@Suite struct MainWindowSnapshotHelperTests {
  @Test func turnsMergeConsecutiveSegmentsOfOneSpeaker() {
    let segments = SampleData.segments()
    let names: (UUID?) -> String = { $0 == nil ? "Unknown" : "Someone" }
    let turns = MainWindowSnapshots.turns(segments, displayName: names)
    #expect(turns.count == 3, "two speakers plus an unassigned segment")
    #expect(turns.map(\.speakerName) == ["Someone", "Someone", "Unknown"])

    var extra = segments[1]
    extra.id = UUID()
    extra.start = 5.6
    extra.end = 6
    extra.text = "Und Montag."
    let grouped = MainWindowSnapshots.turns([segments[0], segments[1], extra], displayName: names)
    #expect(grouped.count == 2)
    #expect(grouped[1].text == "Ich prüfe das Budget bis Freitag. Und Montag.")
    #expect(grouped[1].startSeconds == 2.5)
    #expect(grouped[1].endSeconds == 6)
  }

  @Test func bulletsSplitTheLeadFromTheTextAndDropBoldMarkers() {
    #expect(
      MainWindowSnapshots.bullet("**Fokus**: **Nicolai** prüft die Zahlen.")
        == .init(lead: "Fokus", text: "Nicolai prüft die Zahlen."))
    #expect(
      MainWindowSnapshots.bullet("Nur Text mit **Name**.")
        == .init(lead: "", text: "Nur Text mit Name."))
    #expect(MainWindowSnapshots.bullet("") == .init(lead: "", text: ""))
  }

  @Test func theBannerSplitsAtItsFirstSentence() {
    let parts = MainWindowSnapshots.bannerParts(SetupCopy.bannerEndpointMissing)
    #expect(parts.title == "Summaries are off.")
    #expect(parts.body == "Steno has no LLM endpoint yet, so meetings keep a raw transcript.")
    #expect(MainWindowSnapshots.bannerParts("No stop here").title == "No stop here")
  }

  @Test func daysAndColoursAreStable() {
    #expect(MainWindowSnapshots.dayString(SampleData.startedAt, calendar: utc) == "2026-09-24")
    let index = MainWindowSnapshots.colorIndex(for: SampleData.personNicolaiID)
    #expect(index == MainWindowSnapshots.colorIndex(for: SampleData.personNicolaiID))
    #expect((0..<MainWindowSnapshots.peoplePaletteSize).contains(index))
    #expect(
      (0..<MainWindowSnapshots.peoplePaletteSize).contains(
        MainWindowSnapshots.colorIndex(for: UUID())))
  }
}

// MARK: - Snapshots over the preview store

@Suite @MainActor struct MainWindowSnapshotsTests {
  /// The rich seed: the sample meeting on the 24th, two meetings on the
  /// 23rd (one processing), two on the 22nd (one failed), grouped on a UTC
  /// calendar, with the nav counts, the tag counts, the first bullet as the
  /// preview (none for the processing one) and the sample meeting's two
  /// speakers as chips.
  @Test func theListGroupsTheRichSeedByDayWithPreviewsAndChips() async throws {
    let environment = try await TestSupport.environment(fixtures: .rich)
    let list = MeetingListViewModel(store: environment.store, clock: ManualClock(), calendar: utc)
    let observing = Task { await list.observe() }
    defer { observing.cancel() }
    await eventually("five meetings") { list.all.count == 5 }
    await eventually("speaker chips") { list.speakersByMeeting[SampleData.meetingID] != nil }

    let snapshot = MeetingsListSnapshot(list: list)
    #expect(snapshot.filter == .all)
    #expect(snapshot.tagFilter == nil)
    #expect(snapshot.query == "")
    #expect(snapshot.counts == .init(all: 5, processing: 1, ready: 3, failed: 1))
    #expect(
      snapshot.tags == [
        .init(name: "hiring", count: 1), .init(name: "q4", count: 1),
        .init(name: "strategie", count: 1), .init(name: "team", count: 1),
      ])
    #expect(snapshot.groups.map(\.day) == ["2026-09-24", "2026-09-23", "2026-09-22"])
    #expect(snapshot.groups.map { $0.meetings.count } == [1, 2, 2])

    let sample = try #require(snapshot.groups.first?.meetings.first)
    #expect(sample.id == SampleData.meetingID)
    #expect(sample.title == "Produktstrategie 90/10")
    #expect(sample.startedAt == SampleData.startedAt)
    #expect(sample.durationSeconds == 6)
    #expect(sample.source == .call)
    #expect(sample.state == .ready)
    #expect(sample.failureReason == nil)
    #expect(sample.preview == "Fokus: Speaker 1 schlägt vor, 90 Prozent auf den Kern zu setzen.")
    #expect(sample.hasSummary)
    #expect(sample.tags == ["strategie", "q4"])
    let chips = Dictionary(uniqueKeysWithValues: sample.speakers.map { ($0.id, $0) })
    #expect(chips.count == 2)
    #expect(chips[SampleData.speakerOneID]?.initial == "N")
    #expect(chips[SampleData.speakerOneID]?.isConfirmed == true)
    #expect(
      chips[SampleData.speakerOneID]?.colorIndex
        == MainWindowSnapshots.colorIndex(for: SampleData.personNicolaiID))
    #expect(chips[SampleData.speakerTwoID]?.initial == "J", "the suggested person's letter")
    #expect(chips[SampleData.speakerTwoID]?.isConfirmed == false)

    let failed = try #require(snapshot.groups[2].meetings.first { $0.state == .failed })
    #expect(failed.preview == nil)
    #expect(failed.hasSummary == false)
    #expect(failed.failureReason?.hasPrefix("The LLM endpoint did not answer.") == true)

    // A processing meeting carries no preview: the page shows the stage from
    // the `progress` topic, so a progress tick never re-publishes the list.
    let processing = try #require(snapshot.groups[1].meetings.first { $0.state == .processing })
    #expect(processing.preview == nil)

    list.stateFilter = .failed
    list.selection = SampleData.meetingID
    let filtered = MeetingsListSnapshot(list: list)
    #expect(filtered.filter == .failed)
    #expect(filtered.groups.map { $0.meetings.map(\.id) } == [[failed.id]])
    #expect(filtered.counts.all == 5, "counts ignore the filter")
    #expect(filtered.selection == SampleData.meetingID, "a filter never drops the selection")
  }

  /// The sample meeting's detail: sections with the speaker names
  /// substituted and the bold markers gone, the speaker rows with their
  /// assignments, the transcript as three turns, the retention kind the
  /// view model derives, the tasks, decisions, notes and the export line.
  @Test func theDetailCarriesSectionsSpeakersTurnsAndRetention() async throws {
    let environment = try await TestSupport.environment()
    let detail = MeetingDetailViewModel(meetingID: SampleData.meetingID, environment: environment)
    #expect(MeetingDetailSnapshot(detail: detail) == nil, "nothing before the export loads")
    let tasks = [Task { await detail.observe() }, Task { await detail.observeDeliveries() }]
    defer { for task in tasks { task.cancel() } }
    await eventually("export and speakers loaded") {
      detail.export != nil && detail.speakers.rows.count == 2
    }

    let snapshot = try #require(MeetingDetailSnapshot(detail: detail))
    #expect(snapshot.id == SampleData.meetingID)
    #expect(snapshot.title == "Produktstrategie 90/10")
    #expect(snapshot.language == "de")
    #expect(snapshot.source == .call)
    #expect(snapshot.state == .ready)
    #expect(snapshot.tags == ["strategie", "q4"])
    #expect(snapshot.tab == .summary)
    #expect(snapshot.templateID == SummaryTemplate.defaultID)
    #expect(snapshot.templates.map(\.id) == SummaryTemplate.bundledIDs)

    #expect(snapshot.summaryStatus.kind == .present)
    #expect(snapshot.summary.map(\.heading) == ["Executive Summary", "Offene Fragen"])
    #expect(
      snapshot.summary[0].bullets.first
        == .init(lead: "Fokus", text: "Nicolai schlägt vor, 90 Prozent auf den Kern zu setzen."))
    #expect(snapshot.decisions == ["90/10-Aufteilung wird umgesetzt."])

    let speakers = Dictionary(uniqueKeysWithValues: snapshot.speakers.map { ($0.id, $0) })
    let one = try #require(speakers[SampleData.speakerOneID])
    #expect(one.clusterLabel == "Speaker 1")
    #expect(one.displayName == "Nicolai")
    #expect(one.assignment == .confirmed)
    #expect(one.personID == SampleData.personNicolaiID)
    #expect(one.suggestionName == nil)
    #expect(one.colorIndex == MainWindowSnapshots.colorIndex(for: SampleData.personNicolaiID))
    #expect(!one.isPlaying)
    let two = try #require(speakers[SampleData.speakerTwoID])
    #expect(two.displayName == "Jérôme")
    #expect(two.assignment == .suggested)
    #expect(two.personID == SampleData.personJeromeID)
    #expect(two.suggestionName == "Jérôme", "the suggested name pre-fills the picker")
    #expect(!two.hasClip, "the fixture's clip path is not on disk")

    #expect(snapshot.transcript.map(\.speakerName) == ["Nicolai", "Jérôme", "Unknown"])
    #expect(snapshot.transcript.map(\.startSeconds) == [0, 2.5, 5.5])
    #expect(snapshot.transcript[0].text == "Wir setzen neunzig Prozent auf den Kern.")

    // The fixture asset has a finite rule and an expiry stamp; whether its
    // master is on disk decides between "deletes on" and "deleted".
    if snapshot.retention.filesExist {
      #expect(snapshot.retention.kind == .deletesOn)
      #expect(snapshot.retention.deletesAt == SampleData.audioAsset().expiresAt)
    } else {
      #expect(snapshot.retention.kind == .deleted)
    }
    #expect(!snapshot.retention.keepsAudio)

    #expect(snapshot.tasks.map(\.text) == ["Budgetzahlen prüfen"])
    #expect(snapshot.tasks[0].assigneeName == "Jérôme")
    #expect(snapshot.tasks[0].priority == .high)
    #expect(!snapshot.tasks[0].done)
    #expect(snapshot.notes == "Nachfassen wegen Budget.")
    #expect(snapshot.export.status == .notConfigured)
    #expect(snapshot.export.message == SetupCopy.notExportedNoVault)
    #expect(!snapshot.export.canReexport)
    #expect(!snapshot.canRerunSummary, "no endpoint in the preview environment")
    #expect(!snapshot.isBusy)
    #expect(snapshot.error == nil)

    detail.tab = .scratchpad
    #expect(MeetingDetailSnapshot(detail: detail)?.tab == .notes)
  }

  /// Idle is the empty snapshot; a running recording carries its start,
  /// mode and meeting; a stop returns to idle.
  @Test func theRecordingSnapshotFollowsTheRecorder() async throws {
    let environment = try await TestSupport.environment()
    let recorder = RecordingController(environment: environment)
    #expect(RecordingSnapshot(recorder: recorder) == RecordingSnapshot(state: .idle))

    await recorder.start(mode: .call)
    let live = RecordingSnapshot(recorder: recorder)
    #expect(live.state == .recording)
    #expect(live.startedAt == TestSupport.now)
    #expect(live.mode == .call)
    #expect(live.callApp == nil)
    #expect(live.meetingID != nil)
    #expect(live.meetingID == recorder.activeMeetingID)
    #expect(live.autoStop == nil)
    #expect(live.deniedPermissions.isEmpty)
    #expect(live.error == nil)

    await recorder.stop()
    let stopped = RecordingSnapshot(recorder: recorder)
    #expect(stopped.state == .idle)
    #expect(stopped.meetingID == nil)
    #expect(stopped.level == nil)
  }

  @Test func theAppSnapshotCarriesTheBannerUntilDismissed() async throws {
    let environment = try await TestSupport.environment()
    let controller = AppController(environment: environment)
    #expect(
      AppSnapshot(controller: controller, appearance: .dark, version: "1.2.3").setupBanner == nil,
      "nothing before the first settings emission")
    await controller.launch()
    defer { Task { await controller.shutdown() } }
    await eventually("settings observed") { controller.storedSettings != nil }

    let shown = AppSnapshot(controller: controller, appearance: .dark, version: "1.2.3")
    #expect(shown.version == "1.2.3")
    #expect(shown.appearance == .dark)
    #expect(shown.phone == nil)
    let banner = try #require(shown.setupBanner)
    #expect(banner.title == "Summaries and export are off.")
    #expect(banner.offersSummaries)
    #expect(banner.offersVault)
    #expect(
      AppSnapshot(controller: controller, appearance: .light, hasMeetings: false).setupBanner
        == nil,
      "an empty store shows no banner")

    controller.dismissSetupBanner()
    #expect(AppSnapshot(controller: controller, appearance: .light).setupBanner == nil)
    controller.requestedMeetingID = SampleData.meetingID
    #expect(
      AppSnapshot(controller: controller, appearance: .light).requestedMeetingID
        == SampleData.meetingID)
  }
}

// MARK: - Command routing

@Suite @MainActor struct MainWindowBridgeTests {
  private func bridge(_ environment: AppEnvironment, confirm: Bool = true) -> MainWindowBridge {
    MainWindowBridge(controller: AppController(environment: environment), confirm: { _ in confirm })
  }

  @Test func openURLAcceptsOnlyWebAndMailLinks() async throws {
    let host = bridge(try await TestSupport.environment())
    for url in ["file:///etc/hosts", "javascript:alert(1)", "http://example.com/", "not a url"] {
      let reply = await BridgeDispatcher.dispatch(
        request("r", "system.openURL", ["url": url]), host: host)
      #expect(reply.error?.code == .invalidParams, Comment(rawValue: url))
    }
    let missing = await BridgeDispatcher.dispatch(request("r", "system.openURL"), host: host)
    #expect(missing.error?.code == .invalidParams)
  }

  @Test func methodsOfOtherWindowsAndOfNoSelectionAreTypedErrors() async throws {
    let host = bridge(try await TestSupport.environment())
    let settings = await BridgeDispatcher.dispatch(
      request("r1", "settings.general.setLaunchAtLogin", ["value": true]), host: host)
    #expect(settings.error?.code == .unknownMethod)
    let tab = await BridgeDispatcher.dispatch(
      request("r2", "meeting.setTab", ["tab": "notes"]), host: host)
    #expect(tab.error?.code == .notFound, "no meeting is selected before the list fills")
    let layout = await BridgeDispatcher.dispatch(
      request("r3", "page.layout", ["window": "main", "width": 1200, "height": 760]), host: host)
    #expect(layout == BridgeReply(id: "r3"))
    #expect(host.layout == PageLayoutParams(window: .main, width: 1200, height: 760))
  }

  /// `page.ready` publishes every topic once; `meetings.setFilter` reaches
  /// the list model and the list topic is published again with the new
  /// filter, the selection kept.
  @Test func setFilterChangesTheListAndRepublishes() async throws {
    let host = bridge(try await TestSupport.environment())
    let sink = RecordingSink()
    host.attach(sink)
    let running = Task { await host.run() }
    defer { running.cancel() }
    await eventually("the list observed the store") { host.list.all.count == 1 }
    #expect(sink.events.isEmpty, "nothing is sent before the page is ready")

    let ready = await BridgeDispatcher.dispatch(request("r1", "page.ready"), host: host)
    #expect(ready == BridgeReply(id: "r1"))
    #expect(Array(sink.events.prefix(5).map(\.topic)) == MainWindowBridge.topics)
    #expect(sink.last(.meetingsList)?["filter"] == .string("all"))
    #expect(
      sink.last(.meetingsList)?["selection"] == .string(SampleData.meetingID.uuidString),
      "the first fill selects the newest meeting")
    #expect(host.detail?.id == SampleData.meetingID)
    #expect(sink.last(.recording)?["state"] == .string("idle"))
    await eventually("the detail loaded and published") {
      sink.last(.meetingDetail)?["id"] == .string(SampleData.meetingID.uuidString)
    }

    let before = sink.events.count
    let reply = await BridgeDispatcher.dispatch(
      request("r2", "meetings.setFilter", ["filter": "failed"]), host: host)
    #expect(reply == BridgeReply(id: "r2"))
    #expect(host.list.stateFilter == .failed)
    await eventually("the list republished") {
      sink.events.count > before && sink.last(.meetingsList)?["filter"] == .string("failed")
    }
    #expect(sink.last(.meetingsList)?["groups"] == .array([]))
    #expect(sink.last(.meetingsList)?["selection"] == .string(SampleData.meetingID.uuidString))

    let setTab = await BridgeDispatcher.dispatch(
      request("r3", "meeting.setTab", ["tab": "notes"]), host: host)
    #expect(setTab == BridgeReply(id: "r3"))
    #expect(host.detail?.tab == .scratchpad)
    await eventually("the detail republished") {
      sink.last(.meetingDetail)?["tab"] == .string("notes")
    }
  }

  /// A declined confirmation leaves the meeting; a confirmed one deletes it
  /// through the list model.
  @Test func deleteAsksFirst() async throws {
    let environment = try await TestSupport.environment()
    let declining = bridge(environment, confirm: false)
    let running = Task { await declining.run() }
    defer { running.cancel() }
    await eventually("the list observed the store") { declining.list.all.count == 1 }
    let declined = await BridgeDispatcher.dispatch(
      request("r1", "meetings.delete", ["meetingID": SampleData.meetingID.uuidString]),
      host: declining)
    #expect(declined.result?["confirmed"] == .bool(false))
    #expect(declining.list.all.count == 1)
    let unknown = await BridgeDispatcher.dispatch(
      request("r2", "meetings.delete", ["meetingID": UUID().uuidString]), host: declining)
    #expect(unknown.error?.code == .notFound)

    let confirming = bridge(environment, confirm: true)
    let running2 = Task { await confirming.run() }
    defer { running2.cancel() }
    await eventually("the second list observed the store") { confirming.list.all.count == 1 }
    let confirmed = await BridgeDispatcher.dispatch(
      request("r3", "meetings.delete", ["meetingID": SampleData.meetingID.uuidString]),
      host: confirming)
    #expect(confirmed.result?["confirmed"] == .bool(true))
    await eventually("the meeting left the store") { confirming.list.all.isEmpty }
  }

  /// `-steno-empty`: over a store without meetings, `page.ready` publishes
  /// every topic (the list with no groups and no selection, the detail as
  /// `null`, the app without the setup banner) and nothing traps. The first
  /// meeting to arrive fills the list and selects itself, and flips the app
  /// topic once so the banner shows; a later list change that keeps the
  /// list filled leaves `app` alone.
  @Test func anEmptyStorePublishesEveryTopicAndTheFirstMeetingFlipsTheApp() async throws {
    let environment = try await TestSupport.environment(seed: false)
    let host = bridge(environment)
    let sink = RecordingSink()
    host.attach(sink)
    await host.controller.launch()
    defer { Task { await host.controller.shutdown() } }
    await eventually("settings observed") { host.controller.storedSettings != nil }
    let running = Task { await host.run() }
    defer { running.cancel() }

    let ready = await BridgeDispatcher.dispatch(request("r1", "page.ready"), host: host)
    #expect(ready == BridgeReply(id: "r1"))
    #expect(Array(sink.events.prefix(5).map(\.topic)) == MainWindowBridge.topics)
    #expect(sink.last(.meetingsList)?["groups"] == .array([]))
    #expect(sink.last(.meetingsList)?["tags"] == .array([]))
    #expect((sink.last(.meetingsList)?["selection"] ?? .null) == .null)
    #expect(sink.last(.meetingDetail) == .null)
    #expect((sink.last(.app)?["setupBanner"] ?? .null) == .null, "no banner over an empty store")
    #expect(sink.last(.recording)?["state"] == .string("idle"))
    #expect(host.detail == nil)
    #expect(host.list.error == nil)

    try await environment.store.save(SampleData.meeting())
    await eventually("the first meeting listed and selected itself") {
      sink.last(.meetingsList)?["selection"] == .string(SampleData.meetingID.uuidString)
    }
    await eventually("the app topic flipped and carries the banner") {
      (sink.last(.app)?["setupBanner"] ?? .null) != .null
    }
    #expect(host.detail?.id == SampleData.meetingID)

    let appPublishes = sink.events.filter { $0.topic == .app }.count
    let filtered = await BridgeDispatcher.dispatch(
      request("r2", "meetings.setFilter", ["filter": "failed"]), host: host)
    #expect(filtered == BridgeReply(id: "r2"))
    await eventually("the list republished") {
      sink.last(.meetingsList)?["filter"] == .string("failed")
    }
    #expect(
      sink.events.filter { $0.topic == .app }.count == appPublishes,
      "a list change that keeps the list filled does not re-publish app")
  }

  /// Notes carry their meeting id. A save for the selected meeting goes
  /// through its detail model and waits for the debounce; one for another
  /// meeting (typed before the selection moved, saved after) is written to
  /// that meeting at once and never touches the selection. A flush names
  /// its meeting too, so another meeting's flush never writes the
  /// selection's pending text; an unknown meeting is `notFound`.
  @Test func notesLandOnTheMeetingTheyWereTypedFor() async throws {
    let environment = try await TestSupport.environment(fixtures: .rich)
    let host = bridge(environment)
    let running = Task { await host.run() }
    defer { running.cancel() }
    await eventually("the newest meeting is selected") { host.detail?.id == SampleData.meetingID }
    let other = try #require(host.list.all.first { $0.id != SampleData.meetingID })
    let original = try #require(try await environment.store.meeting(id: SampleData.meetingID))

    let late = await BridgeDispatcher.dispatch(
      request(
        "r1", "meeting.saveNotes", ["meetingID": other.id.uuidString, "text": "Late note"]),
      host: host)
    #expect(late == BridgeReply(id: "r1"))
    #expect(try await environment.store.meeting(id: other.id)?.scratchpad == "Late note")
    #expect(
      try await environment.store.meeting(id: SampleData.meetingID)?.scratchpad
        == original.scratchpad, "the selection's notes are untouched")

    let mine = await BridgeDispatcher.dispatch(
      request(
        "r2", "meeting.saveNotes",
        ["meetingID": SampleData.meetingID.uuidString, "text": "Mine"]),
      host: host)
    #expect(mine == BridgeReply(id: "r2"))
    #expect(
      try await environment.store.meeting(id: SampleData.meetingID)?.scratchpad
        == original.scratchpad, "the selection's notes wait for the debounce")
    let otherFlush = await BridgeDispatcher.dispatch(
      request("r3", "meeting.flushNotes", ["meetingID": other.id.uuidString]), host: host)
    #expect(otherFlush == BridgeReply(id: "r3"))
    #expect(
      try await environment.store.meeting(id: SampleData.meetingID)?.scratchpad
        == original.scratchpad, "another meeting's flush leaves the selection's text pending")
    let flush = await BridgeDispatcher.dispatch(
      request("r4", "meeting.flushNotes", ["meetingID": SampleData.meetingID.uuidString]),
      host: host)
    #expect(flush == BridgeReply(id: "r4"))
    #expect(try await environment.store.meeting(id: SampleData.meetingID)?.scratchpad == "Mine")
    #expect(try await environment.store.meeting(id: other.id)?.scratchpad == "Late note")

    let unknown = await BridgeDispatcher.dispatch(
      request("r5", "meeting.saveNotes", ["meetingID": UUID().uuidString, "text": "x"]),
      host: host)
    #expect(unknown.error?.code == .notFound)
  }

  /// `meetings.select` for a meeting the list does not have yet (a deep link
  /// racing the store) is no error and no selection: the id waits until
  /// its row is listed, then becomes the selection with its detail model.
  @Test func selectingAnUnlistedMeetingWaitsForItsRow() async throws {
    let environment = try await TestSupport.environment()
    let host = bridge(environment)
    let running = Task { await host.run() }
    defer { running.cancel() }
    await eventually("the newest meeting is selected") { host.detail?.id == SampleData.meetingID }

    var live = SampleData.meeting(state: .recording)
    live.id = UUID()
    live.title = "Just started"
    live.startedAt = SampleData.startedAt.addingTimeInterval(3_600)
    let reply = await BridgeDispatcher.dispatch(
      request("r1", "meetings.select", ["meetingID": live.id.uuidString]), host: host)
    #expect(reply == BridgeReply(id: "r1"))
    #expect(host.list.selection == SampleData.meetingID, "nothing moves before the row exists")
    #expect(host.detail?.id == SampleData.meetingID)

    try await environment.store.save(live)
    await eventually("the row arrived and was selected") { host.list.selection == live.id }
    await eventually("its detail model follows") { host.detail?.id == live.id }
  }

  /// The page's "Clear filters" is three commands: `meetings.setFilter(all)`,
  /// `meetings.setTagFilter(null)` and `meetings.setQuery("")` together show
  /// every meeting again.
  @Test func theThreeFilterSettersClearEveryFilter() async throws {
    let host = bridge(try await TestSupport.environment(fixtures: .rich))
    let running = Task { await host.run() }
    defer { running.cancel() }
    await eventually("five meetings") { host.list.all.count == 5 }

    _ = await BridgeDispatcher.dispatch(
      request("r1", "meetings.setFilter", ["filter": "failed"]), host: host)
    _ = await BridgeDispatcher.dispatch(
      request("r2", "meetings.setTagFilter", ["tag": "q4"]), host: host)
    _ = await BridgeDispatcher.dispatch(
      request("r3", "meetings.setQuery", ["query": "budget"]), host: host)
    #expect(host.list.stateFilter == .failed)
    #expect(host.list.tagFilter == "q4")
    #expect(host.list.query == "budget")
    #expect(host.list.meetings.count < 5)

    let filter = await BridgeDispatcher.dispatch(
      request("r4", "meetings.setFilter", ["filter": "all"]), host: host)
    let tag = await BridgeDispatcher.dispatch(request("r5", "meetings.setTagFilter"), host: host)
    let query = await BridgeDispatcher.dispatch(
      request("r6", "meetings.setQuery", ["query": ""]), host: host)
    #expect(filter == BridgeReply(id: "r4"))
    #expect(tag == BridgeReply(id: "r5"))
    #expect(query == BridgeReply(id: "r6"))
    #expect(host.list.stateFilter == .all)
    #expect(host.list.tagFilter == nil)
    #expect(host.list.query == "")
    #expect(host.list.searchHits == nil)
    #expect(host.list.meetings.count == 5)
  }
}
