import Foundation
import StenoCore

/// One realistic value per contract type. `BridgeFixturesTests` encodes them
/// into `apps/macos/web/fixtures/bridge/`, the web tests parse those files,
/// and the page's mock transport serves them, so the sample meeting the web
/// UI shows in development is the one the Mac's `SampleData` seeds.
public enum BridgeSamples {
  public static func uuid(_ n: Int) -> UUID { SampleData.uuid(n) }

  /// 2026-09-29 14:50 in Europe/Berlin (UTC+2).
  public static let startedAt = Date(timeIntervalSince1970: 1_790_686_200)
  public static let meetingID = SampleData.meetingID
  public static let failedMeetingID = uuid(68)
  public static let speakerNicolai = SampleData.speakerOneID
  public static let speakerJerome = SampleData.speakerTwoID
  public static let speakerAnna = uuid(22)
  public static let speakerUnknown = uuid(23)
  public static let personNicolai = SampleData.personNicolaiID
  public static let personJerome = SampleData.personJeromeID
  public static let personAnna = uuid(12)
  public static let phoneID = uuid(40)

  // MARK: Snapshots

  public static let app = AppSnapshot(
    version: "0.10.0",
    setupBanner: .init(
      title: "Summaries are off.",
      body:
        "Choose an AI service and Steno writes a summary and tasks for every meeting. Only the transcript text is sent.",
      offersSummaries: true, offersVault: true),
    phone: .init(
      name: "Nicolai's iPhone", lastSyncAt: startedAt.addingTimeInterval(-120), isReachable: true),
    requestedMeetingID: nil, requestedSettingsSection: nil)

  public static let recordingIdle = RecordingSnapshot(state: .idle)

  public static let recordingLive = RecordingSnapshot(
    state: .recording, startedAt: startedAt, mode: .call, callApp: "Zoom", meetingID: meetingID,
    level: .init(mic: 0.42, system: 0.18),
    autoStop: .init(remainingSeconds: 42, totalSeconds: 60, reason: "Zoom closed"),
    deniedPermissions: [], warning: nil, error: nil)

  public static let progress = ProgressSnapshot(entries: [
    .init(
      meetingID: uuid(2), stage: "transcribing", title: "Transcribing", fraction: 0.62,
      estimatedRemainingSeconds: 95)
  ])

  static let chips: [SpeakerChip] = [
    .init(id: speakerNicolai, initial: "N", colorIndex: 0, isConfirmed: true),
    .init(id: speakerJerome, initial: "J", colorIndex: 1, isConfirmed: true),
    .init(id: speakerAnna, initial: "A", colorIndex: 2, isConfirmed: true),
    .init(id: speakerUnknown, initial: "?", colorIndex: 3, isConfirmed: false),
  ]

  public static let meetingsList = MeetingsListSnapshot(
    filter: .all, tagFilter: nil, query: "",
    counts: .init(all: 3, processing: 0, ready: 2, failed: 1),
    tags: [.init(name: "strategie", count: 1), .init(name: "q4", count: 1)],
    groups: [
      .init(
        day: "2026-09-29",
        meetings: [
          MeetingRow(
            id: meetingID, title: "Produktstrategie 90/10", startedAt: startedAt,
            durationSeconds: 2738, source: .call, state: .ready,
            preview:
              "Nicolai schlägt vor, 90 Prozent auf den Kern zu setzen. Jérôme prüft die Zahlen bis Freitag.",
            hasSummary: true, speakers: chips, tags: ["strategie", "q4"]),
          MeetingRow(
            id: uuid(3), title: "Tuesday 10:08", startedAt: startedAt.addingTimeInterval(-17_000),
            durationSeconds: 2291, source: .inPerson, state: .ready, preview: nil,
            hasSummary: false,
            speakers: [], tags: []),
        ]),
      .init(
        day: "2026-09-28",
        meetings: [
          MeetingRow(
            id: failedMeetingID, title: "Investor update prep",
            startedAt: startedAt.addingTimeInterval(-83_000), durationSeconds: 1650, source: .call,
            state: .failed, failureReason: "Transcription failed: model not installed",
            preview: nil,
            hasSummary: false, speakers: [], tags: ["investors"])
        ]),
    ],
    selection: meetingID)

  public static let meetingDetail = MeetingDetailSnapshot(
    id: meetingID, title: "Produktstrategie 90/10", startedAt: startedAt, durationSeconds: 2738,
    language: "de", source: .call, state: .ready, tags: ["strategie", "q4"], tab: .summary,
    retention: .init(
      kind: .deletesOn, deletesAt: startedAt.addingTimeInterval(30 * 86_400), keepsAudio: false,
      showsKeepToggle: true, filesExist: true),
    speakers: [
      .init(
        id: speakerNicolai, clusterLabel: "Speaker 1", displayName: "Nicolai",
        assignment: .confirmed,
        personID: personNicolai, email: "nicolai@example.com", colorIndex: 0, hasClip: true,
        isPlaying: false),
      .init(
        id: speakerJerome, clusterLabel: "Speaker 2", displayName: "Jérôme", assignment: .confirmed,
        personID: personJerome, colorIndex: 1, hasClip: true, isPlaying: false),
      .init(
        id: speakerAnna, clusterLabel: "Speaker 3", displayName: "Anna", assignment: .suggested,
        personID: personAnna, suggestionName: "Anna", colorIndex: 2, hasClip: true, isPlaying: false
      ),
      .init(
        id: speakerUnknown, clusterLabel: "Speaker 4", displayName: "Speaker 4",
        assignment: .unknown,
        colorIndex: 3, hasClip: false, isPlaying: false),
    ],
    templates: [
      .init(id: "default", name: "Meeting notes"), .init(id: "standup", name: "Standup"),
    ],
    templateID: "default",
    summaryStatus: .init(kind: .present),
    summary: [
      .init(
        id: "executive-summary", heading: "Executive summary",
        bullets: [
          .init(
            lead: "Fokus",
            text:
              "Nicolai schlägt vor, 90 Prozent der Kapazität auf den Kern zu setzen und Nebenprojekte bis Q1 zu pausieren."
          ),
          .init(
            lead: "Budget",
            text: "Jérôme prüft die Zahlen bis Freitag und bringt zwei Szenarien mit."),
        ]),
      .init(
        id: "open-questions", heading: "Open questions",
        bullets: [
          .init(
            lead: "Zeitplan",
            text: "Start im Oktober oder erst im November nach dem Investor-Update?")
        ]),
    ],
    transcript: [
      .init(
        id: uuid(100), speakerID: speakerNicolai, speakerName: "Nicolai", startSeconds: 12,
        endSeconds: 41,
        text:
          "Lass uns kurz auf die Prioritäten schauen. Ich würde vorschlagen, dass wir neunzig Prozent auf den Kern setzen."
      ),
      .init(
        id: uuid(101), speakerID: speakerJerome, speakerName: "Jérôme", startSeconds: 41,
        endSeconds: 65,
        text:
          "Das geht nur, wenn wir das Budget entsprechend umschichten. Ich rechne bis Freitag zwei Szenarien."
      ),
      .init(
        id: uuid(102), speakerID: speakerUnknown, speakerName: "Speaker 4", startSeconds: 65,
        endSeconds: 90, text: "Die Partner sollten das nicht aus zweiter Hand hören."),
    ],
    tasks: [
      .init(
        id: uuid(200), text: "Zahlen für beide Szenarien", assigneeName: "Jérôme",
        assigneeColorIndex: 1,
        dueDate: startedAt.addingTimeInterval(3 * 86_400), priority: .normal, done: false),
      .init(
        id: uuid(201), text: "Entscheidung im Investor-Update ansprechen", assigneeName: "Nicolai",
        assigneeColorIndex: 0, dueDate: nil, priority: .high, done: true),
    ],
    decisions: [
      "Wir setzen neunzig Prozent auf den Kern. Nebenprojekte pausieren bis zur Q1-Planung."
    ],
    notes: "",
    export: .init(
      status: .notConfigured, message: "Not exported: no Obsidian vault is configured.",
      canReexport: false, canReveal: false),
    canProcessAgain: false, canRerunSummary: true, isBusy: false)

  /// `meetingDetail` ready and exported, its recording kept because the
  /// speakers or the transcript may be incomplete, so Process again is
  /// offered. Only the Rust host sends this kind.
  public static let meetingDetailKeptIncomplete: MeetingDetailSnapshot = {
    var detail = meetingDetail
    detail.retention = .init(
      kind: .keptIncomplete, deletesAt: nil, keepsAudio: false, showsKeepToggle: true,
      filesExist: true)
    detail.canProcessAgain = true
    return detail
  }()

  public static let settingsGeneral = GeneralSettingsSnapshot(
    subtitle: "Steno 0.10.0", version: "0.10.0", loginItem: .enabled, detectionEnabled: true,
    defaultTemplateID: "default",
    templates: [
      .init(
        id: "default", name: "Standard",
        description: "Executive summary, decisions, open questions and the tasks."),
      .init(
        id: "standup", name: "Standup",
        description: "What each person did, does next and is blocked by."),
      .init(
        id: "oneOnOne", name: "One-on-one",
        description: "Topics raised, agreements and follow-ups for two people."),
    ],
    calendarPermission: .granted, requestingCalendar: false,
    updates: .init(
      canCheck: true, automaticallyChecks: true, automaticallyDownloads: false,
      lastCheckAt: startedAt.addingTimeInterval(-3_600), outcome: .upToDate),
    acknowledgements: [
      .init(
        group: .speechModels, name: "Parakeet TDT 0.6B v3 (int8)", licence: "CC-BY-4.0",
        source: "FluidInference/parakeet-tdt-0.6b-v3-coreml"),
      .init(
        group: .speechModels, name: "Speaker diarization (pyannote community-1)",
        licence: "Apache-2.0 (pyannote and WeSpeaker upstream)",
        source: "FluidInference/speaker-diarization-coreml"),
      .init(
        group: .libraries, name: "Sparkle", licence: "MIT",
        source: "https://github.com/sparkle-project/Sparkle"),
      .init(
        group: .libraries, name: "GRDB.swift", licence: "MIT",
        source: "https://github.com/groue/GRDB.swift"),
    ])

  public static let settingsRecording = RecordingSettingsSnapshot(
    subtitle: "Ready",
    devices: [
      .init(uid: "BuiltInMicrophoneDevice", name: "MacBook Pro Microphone"),
      .init(uid: "AirPodsPro", name: "Nicolai's AirPods Pro"),
    ],
    inputDeviceUID: nil, audioFolderPath: "/Users/nicolai/Library/Application Support/Steno/audio",
    audioFolderName: "audio", folderUsage: .measured, folderUsageBytes: 734_003_200,
    retention: .init(mode: .keepDays, days: 30),
    retentionFootnote:
      "Each recording is deleted 30 days after it was processed and exported. Transcripts, summaries and exports are never deleted by this rule.",
    keptForeverCount: 2,
    permissions: [
      .init(kind: .microphone, state: .granted, isRequesting: false),
      .init(kind: .systemAudio, state: .granted, isRequesting: false),
    ])

  public static let settingsTranscription = TranscriptionSettingsSnapshot(
    subtitle: "Ready", engineID: "parakeet",
    engines: [.init(id: "parakeet", name: "Parakeet"), .init(id: "whisper", name: "Whisper")],
    showsEnginePicker: true,
    assets: [
      .init(
        id: "parakeetV3", name: "Parakeet v3", detail: "Speech to text, 25 languages",
        state: .installed, installedBytes: 485_000_000),
      .init(
        id: "offlineDiarizer", name: "Speaker separation", detail: "Who spoke when",
        state: .downloading, downloadFraction: 0.35, downloadPhase: "Downloading"),
    ],
    allInstalled: false)

  public static let settingsSummaries = SummariesSettingsSnapshot(
    subtitle: "Not set up",
    presets: [
      .init(
        id: "lmStudio", title: "LM Studio on this Mac", needsAPIKey: false, showsServerField: true,
        modelPlaceholder: "the model loaded in LM Studio"),
      .init(
        id: "codex", title: "ChatGPT (Codex)", needsAPIKey: false, showsServerField: false,
        modelPlaceholder: "pick a model"),
      .init(
        id: "openAI", title: "OpenAI", needsAPIKey: true, showsServerField: false,
        modelPlaceholder: "gpt-4.1-mini"),
    ],
    presetID: "lmStudio", baseURL: "http://127.0.0.1:1234/v1", model: "", contextTokens: "32000",
    hasAPIKey: false, isConfigured: false, isTesting: false)

  /// The ChatGPT preset after confirmation, with the model list: what the
  /// `codex` scenario serves as `settings.summaries`.
  public static let settingsSummariesCodex = SummariesSettingsSnapshot(
    subtitle: "ChatGPT (Codex)", presets: settingsSummaries.presets, presetID: "codex",
    baseURL: "http://127.0.0.1:1234/v1", model: "", contextTokens: "32000", hasAPIKey: false,
    isConfigured: true, isTesting: false,
    testResult: .init(ok: true, message: "Connected. 3 models listed; structured output works."),
    codex: .init(
      confirmed: true, signIn: .signedIn, signInDetail: "nicolai@example.com (Plus)",
      model: "gpt-5.1-codex",
      models: [
        .init(slug: "gpt-5.1-codex", name: "GPT-5.1 Codex"),
        .init(slug: "gpt-5.1-codex-mini", name: "GPT-5.1 Codex mini"),
      ]))

  /// The OpenAI preset with a saved key the host keeps in the secrets
  /// file: what the Rust host sends on Linux without a keyring.
  public static let settingsSummariesFileKey = SummariesSettingsSnapshot(
    subtitle: "OpenAI", presets: settingsSummaries.presets, presetID: "openAI", baseURL: "",
    model: "gpt-4.1-mini", contextTokens: "32000", hasAPIKey: true, isConfigured: true,
    isTesting: false, keyStore: .file)

  public static let settingsExport = ExportSettingsSnapshot(
    subtitle: "Off", enabled: false, vaultPath: nil, vaultName: nil, peopleFolder: "People",
    includeAudio: false, taskTag: "#steno", saved: false)

  public static let settingsPhone = PhoneSettingsSnapshot(
    subtitle: "Nicolai's iPhone", macID: "steno-mac-7f3a",
    devices: [
      .init(
        id: phoneID, name: "Nicolai's iPhone", pairedAt: startedAt.addingTimeInterval(-5 * 86_400),
        lastSeenAt: startedAt.addingTimeInterval(-120))
    ],
    listener: .init(state: .listening, port: 52_431), pairing: nil, receipts: [])

  /// A pairing code open for four minutes and a transfer half received: what
  /// the `pairing` scenario serves as `settings.iphone`. The PNG is a 29 by 29
  /// placeholder in the shape of a code, not a scannable one.
  public static let settingsPhonePairing = PhoneSettingsSnapshot(
    subtitle: "Nicolai's iPhone", macID: "steno-mac-7f3a", devices: settingsPhone.devices,
    listener: .init(state: .listening, port: 52_431),
    pairing: .init(
      expiresAt: startedAt.addingTimeInterval(240),
      qrPNGBase64:
        "iVBORw0KGgoAAAANSUhEUgAAAB0AAAAdCAAAAABz+DjTAAAAtUlEQVR42m1TCRIDIQjL/z+dtmIOnO41chgCcYG5qC/J7wtd5HH8FvMca7yzS9a4TtSRsS8ebe9o10pEmBCyyhe3527KQ6a3LWeYCx3TH9wun410CVlKF+lUcxOohJsPOdAD8GxNCk0TjXHTt0ZUB5u4yzCxtA6PxOhhSc843FCzuq2ZKeHBNbYOT4gny8NBtczSV8oXaLRJPeCPshQnHYOte8/Q5Ku3XrL/hecslpolluuf2AdYtKtjEhSZQwAAAABJRU5ErkJggg=="
    ),
    receipts: [
      .init(
        deviceID: phoneID, recordingID: uuid(41), receivedBytes: 12_500_000, totalBytes: 25_000_000)
    ])

  /// Page 1 with the microphone granted and the test recording running; the
  /// Summaries row carries the Settings sample without a subtitle.
  public static let onboarding = OnboardingSnapshot(
    page: .permissions,
    permissions: [
      .init(
        kind: .microphone, state: .granted, isRequired: true, isRequesting: false, isSkipped: false
      ),
      .init(
        kind: .systemAudio, state: .unknown, isRequired: true, isRequesting: true, isSkipped: false
      ),
      .init(
        kind: .calendar, state: .unknown, isRequired: false, isRequesting: false, isSkipped: false
      ),
      .init(
        kind: .localNetwork, state: .unknown, isRequired: false, isRequesting: false,
        isSkipped: true),
    ],
    permissionsComplete: false,
    setup: [.init(kind: .summaries, state: .open), .init(kind: .vault, state: .open)],
    canSaveSummaries: false, summaries: onboardingSummaries, vault: .init(),
    retentionSentence: onboardingRetention, finished: false)

  static let onboardingRetention =
    "Each recording is deleted 30 days after it was processed and exported. Change this any time in Settings > Recording."

  static var onboardingSummaries: SummariesSettingsSnapshot {
    var summaries = settingsSummaries
    summaries.subtitle = ""
    return summaries
  }

  /// The Summaries form behind a saved row: the model named in the row's
  /// line, configured, as the view model has it after Save.
  static var onboardingSummariesSaved: SummariesSettingsSnapshot {
    var summaries = onboardingSummaries
    summaries.model = "qwen3-8b"
    summaries.isConfigured = true
    return summaries
  }

  /// Page 2 with the Summaries row saved and a vault chosen but refused:
  /// what the `onboarding-setup` scenario and its page-2 screens start from.
  public static let onboardingSetup = OnboardingSnapshot(
    page: .setup,
    permissions: [
      .init(
        kind: .microphone, state: .granted, isRequired: true, isRequesting: false, isSkipped: false
      ),
      .init(
        kind: .systemAudio, state: .granted, isRequired: true, isRequesting: false, isSkipped: false
      ),
      .init(
        kind: .calendar, state: .granted, isRequired: false, isRequesting: false, isSkipped: false
      ),
      .init(
        kind: .localNetwork, state: .unknown, isRequired: false, isRequesting: false,
        isSkipped: true),
    ],
    permissionsComplete: true,
    setup: [
      .init(kind: .summaries, state: .saved, savedLine: "Saved: qwen3-8b at 127.0.0.1"),
      .init(kind: .vault, state: .open),
    ],
    canSaveSummaries: true, summaries: onboardingSummariesSaved,
    vault: .init(
      path: "/Users/nicolai/Notes/Work Vault", name: "Work Vault",
      validationMessage: "The Obsidian vault at /Users/nicolai/Notes/Work Vault does not exist."),
    retentionSentence: onboardingRetention, finished: false)

  // MARK: Envelope

  public static let selectSpeaker = SelectSpeakerParams(
    speakerID: speakerUnknown, option: .init(kind: .person, label: "Anna", personID: personAnna))

  public static let request = BridgeRequest(
    id: "req-1", method: .speakersSelect, params: try? jsonValue(selectSpeaker))

  /// The `JSONValue` form of a typed params value, for envelopes.
  static func jsonValue<T: Encodable>(_ value: T) throws -> JSONValue {
    try BridgeJSON.decode(JSONValue.self, from: BridgeJSON.encode(value))
  }

  public static let reply = BridgeReply(id: "req-1")

  public static let errorReply = BridgeReply(
    id: "req-2", error: .init(code: .notFound, message: "No meeting with that id."))

  public static let event = BridgeEvent(
    topic: .recording,
    payload: .object(["state": .string("idle"), "deniedPermissions": .array([])]))

  // MARK: Commands

  public static let speakerOptions = SpeakerOptionsReply(
    prefill: "Anna",
    options: [
      .init(kind: .person, label: "Anna", detail: "Suggested, 87% match", personID: personAnna),
      .init(kind: .person, label: "Nicolai", personID: personNicolai),
      .init(kind: .create, label: "Add “Anna Berger”"),
      .init(kind: .unknown, label: "Leave unnamed"),
    ])
}

/// Every fixture the contract records, by file name. Snapshots and command
/// types share one catalog so the web side can iterate it.
public struct BridgeFixture: Sendable {
  public let name: String
  public let encode: @Sendable () throws -> Data
  /// Decodes the data as the fixture's type and re-encodes it; the result
  /// must be byte-identical to `encode()` for the round trip to hold.
  public let reencode: @Sendable (Data) throws -> Data

  public init<T: Codable & Sendable>(_ name: String, _ value: T) {
    self.name = name
    encode = { try BridgeJSON.encode(value) }
    reencode = { data in try BridgeJSON.encode(BridgeJSON.decode(T.self, from: data)) }
  }

  /// The bytes written to `<name>.json`: the encoding plus a trailing newline.
  public func fileData() throws -> Data {
    var data = try encode()
    data.append(0x0A)
    return data
  }
}

extension BridgeSamples {
  public static let fixtures: [BridgeFixture] = [
    BridgeFixture("app", app),
    BridgeFixture("recording", recordingIdle),
    BridgeFixture("recording.live", recordingLive),
    BridgeFixture("progress", progress),
    BridgeFixture("meetings.list", meetingsList),
    BridgeFixture("meeting.detail", meetingDetail),
    BridgeFixture("meeting.detail.keptIncomplete", meetingDetailKeptIncomplete),
    BridgeFixture("settings.general", settingsGeneral),
    BridgeFixture("settings.recording", settingsRecording),
    BridgeFixture("settings.transcription", settingsTranscription),
    BridgeFixture("settings.summaries", settingsSummaries),
    BridgeFixture("settings.summaries.codex", settingsSummariesCodex),
    BridgeFixture("settings.summaries.fileKey", settingsSummariesFileKey),
    BridgeFixture("settings.export", settingsExport),
    BridgeFixture("settings.iphone", settingsPhone),
    BridgeFixture("settings.iphone.pairing", settingsPhonePairing),
    BridgeFixture("onboarding", onboarding),
    BridgeFixture("onboarding.setup", onboardingSetup),
    BridgeFixture("envelope.request", request),
    BridgeFixture("envelope.reply", reply),
    BridgeFixture("envelope.error", errorReply),
    BridgeFixture("envelope.event", event),
    BridgeFixture("speakers.options.reply", speakerOptions),
    BridgeFixture("params.page.layout", PageLayoutParams(window: .main, width: 1200, height: 760)),
    BridgeFixture("params.meetings.setFilter", SetFilterParams(filter: .ready)),
    BridgeFixture("params.meetings.setTagFilter", SetTagFilterParams(tag: "q4")),
    BridgeFixture("params.meetings.setQuery", SetQueryParams(query: "budget")),
    BridgeFixture("params.meetingID", MeetingIDParams(meetingID: meetingID)),
    BridgeFixture("params.meeting.setTab", SetTabParams(tab: .transcript)),
    BridgeFixture("params.meeting.setTags", SetTagsParams(tags: ["strategie", "q4"])),
    BridgeFixture("params.meeting.setTemplate", SetTemplateParams(templateID: "standup")),
    BridgeFixture("params.bool", SetBoolParams(value: true)),
    BridgeFixture("params.string", SetStringParams(value: "BuiltInMicrophoneDevice")),
    BridgeFixture(
      "params.meeting.saveNotes",
      SaveNotesParams(meetingID: meetingID, text: "Follow up with Anna on Monday.")),
    BridgeFixture(
      "params.speakers.options", SpeakerOptionsParams(speakerID: speakerUnknown, query: "an")),
    BridgeFixture(
      "params.speakers.select",
      SelectSpeakerParams(
        speakerID: speakerUnknown, option: .init(kind: .person, label: "Anna", personID: personAnna)
      )),
    BridgeFixture("params.speakerID", SpeakerIDParams(speakerID: speakerNicolai)),
    BridgeFixture("params.recording.start", StartRecordingParams(mode: .inPerson)),
    BridgeFixture(
      "params.settings.recording.setRetention",
      SetRetentionParams(retention: RecordingSettingsSnapshot.Retention(mode: .keepDays, days: 30))),
    BridgeFixture("params.permissionKind", PermissionKindParams(kind: .systemAudio)),
    BridgeFixture("params.assetID", AssetIDParams(assetID: "parakeetV3")),
    BridgeFixture(
      "params.settings.general.setAutomaticUpdates",
      SetAutomaticUpdatesParams(automaticallyChecks: true, automaticallyDownloads: false)),
    BridgeFixture(
      "params.settings.summaries.update",
      SummariesUpdateParams(model: "gpt-4.1-mini", apiKey: "sk-…")),
    BridgeFixture(
      "params.settings.export.update", ExportUpdateParams(includeAudio: true, taskTag: "#todo")),
    BridgeFixture("params.deviceID", DeviceIDParams(deviceID: phoneID)),
    BridgeFixture("params.onboarding.setupStep", SetupStepParams(step: .vault)),
    BridgeFixture(
      "params.system.openURL", OpenURLParams(url: "https://github.com/NicolaiSchmid/steno")),
    BridgeFixture("params.window", WindowParams(window: .settings, section: .summaries)),
    BridgeFixture(
      "params.ui.confirmDestructive",
      ConfirmDestructiveParams(
        title: "Delete this meeting?",
        message: "The recording, transcript and summary are removed.",
        confirmTitle: "Delete")),
    BridgeFixture("reply.confirm", ConfirmReply(confirmed: true)),
    BridgeFixture("reply.chosenPath", ChosenPathReply(path: "/Users/nicolai/Notes")),
  ]
}
