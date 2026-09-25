import Foundation

/// Deterministic model values for tests and goldens: sequential UUIDs, fixed
/// `Date(timeIntervalSince1970:)` values, invented text. Never real meeting
/// content.
public enum SampleData {
  /// `00000000-0000-0000-0000-000000000001` for `n == 1`.
  public static func uuid(_ n: Int) -> UUID {
    let (high, low) = (UInt8(truncatingIfNeeded: n >> 8), UInt8(truncatingIfNeeded: n))
    return UUID(uuid: (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, high, low))
  }

  /// 2026-09-24T09:00:00Z.
  public static let startedAt = Date(timeIntervalSince1970: 1_790_240_400)
  public static let createdAt = Date(timeIntervalSince1970: 1_790_244_000)
  public static let updatedAt = Date(timeIntervalSince1970: 1_790_244_300)

  public static let meetingID = uuid(1)
  public static let personNicolaiID = uuid(10)
  public static let personJeromeID = uuid(11)
  public static let speakerOneID = uuid(20)
  public static let speakerTwoID = uuid(21)

  /// A unit vector along `axis`.
  public static func embedding(axis: Int) -> Embedding {
    var values = [Float](repeating: 0, count: Embedding.dimension)
    values[axis % Embedding.dimension] = 1
    return Embedding(values)
  }

  public static func meeting(state: MeetingState = .ready) -> Meeting {
    Meeting(
      id: meetingID,
      title: "Produktstrategie 90/10",
      startedAt: startedAt,
      duration: 6,
      language: Locale.Language(stenoIdentifier: "de"),
      source: .macCall,
      calendarEventID: "event-1",
      tags: ["strategie", "q4"],
      state: state,
      templateID: SummaryTemplate.defaultID,
      summary: summaryDocument(),
      scratchpad: "Nachfassen wegen Budget.",
      llmUsage: LLMUsage(promptTokens: 1200, completionTokens: 300, requests: 3),
      createdAt: createdAt,
      updatedAt: updatedAt
    )
  }

  public static func summaryDocument() -> SummaryDocument {
    SummaryDocument(
      templateID: SummaryTemplate.defaultID,
      language: Locale.Language(stenoIdentifier: "de"),
      sections: [
        SummarySection(
          id: "executive-summary",
          heading: "Executive Summary",
          bullets: [
            SummaryBullet(
              lead: "Fokus", text: "Speaker 1 schlägt vor, 90 Prozent auf den Kern zu setzen."),
            SummaryBullet(lead: "Budget", text: "Speaker 2 will die Zahlen bis Freitag prüfen."),
          ]),
        SummarySection(
          id: "open-questions",
          heading: "Offene Fragen",
          bullets: [SummaryBullet(lead: "Zeitplan", text: "Start im Oktober oder November?")]),
      ])
  }

  /// Ordered by display name, as `MeetingStore.export` returns them.
  public static func persons() -> [Person] {
    [
      Person(
        id: personJeromeID, displayName: "Jérôme", embedding: embedding(axis: 1), sampleCount: 1,
        createdAt: createdAt),
      Person(
        id: personNicolaiID, displayName: "Nicolai", email: "nicolai@example.com",
        embedding: embedding(axis: 0), sampleCount: 3, createdAt: createdAt),
    ]
  }

  /// Ordered by display name, as `MeetingStore.export` returns them.
  public static func participants() -> [Participant] {
    [
      Participant(id: uuid(31), meetingID: meetingID, displayName: "Jérôme", role: .them),
      Participant(
        id: uuid(30), meetingID: meetingID, personID: personNicolaiID, displayName: "Nicolai",
        role: .me, email: "nicolai@example.com"),
    ]
  }

  public static func speakers() -> [Speaker] {
    [
      Speaker(
        id: speakerOneID, meetingID: meetingID, clusterLabel: "Speaker 1",
        assignment: .confirmed(personID: personNicolaiID), embedding: embedding(axis: 0),
        sampleClipRange: 0.5...4.5, sampleClipURL: nil, clusterConfidence: 0.9),
      Speaker(
        id: speakerTwoID, meetingID: meetingID, clusterLabel: "Speaker 2",
        assignment: .suggested(personID: personJeromeID, similarity: 0.72),
        embedding: embedding(axis: 1), sampleClipRange: 3...5.5,
        sampleClipURL: URL(
          fileURLWithPath:
            "/tmp/steno/\(meetingID.uuidString)/speakers/\(speakerTwoID.uuidString).wav"),
        clusterConfidence: 0.75),
    ]
  }

  public static func segments() -> [TranscriptSegment] {
    [
      TranscriptSegment(
        id: uuid(40), meetingID: meetingID, start: 0, end: 2.5, speakerID: speakerOneID, lane: .mic,
        text: "Wir setzen neunzig Prozent auf den Kern.",
        rawText: "wir setzen 90 prozent auf den kern"),
      TranscriptSegment(
        id: uuid(41), meetingID: meetingID, start: 2.5, end: 5.5, speakerID: speakerTwoID,
        lane: .system, text: "Ich prüfe das Budget bis Freitag.",
        rawText: "ich prüfe das budget bis freitag"),
      TranscriptSegment(
        id: uuid(42), meetingID: meetingID, start: 5.5, end: 6, speakerID: nil, lane: .system,
        text: "Gut.", rawText: "gut"),
    ]
  }

  public static func tasks() -> [MeetingTask] {
    [
      MeetingTask(
        id: uuid(50), meetingID: meetingID, text: "Budgetzahlen prüfen",
        assigneePersonID: personJeromeID, assigneeName: "Jérôme", priority: .high,
        dueDate: Date(timeIntervalSince1970: 1_790_640_000), done: false)
    ]
  }

  public static func decisions() -> [Decision] {
    [
      Decision(
        id: MeetingStore.derivedID(meetingID, salt: "decision-0"), meetingID: meetingID,
        text: "90/10-Aufteilung wird umgesetzt.")
    ]
  }

  public static func audioAsset() -> AudioAsset {
    let folder = URL(fileURLWithPath: "/tmp/steno/\(meetingID.uuidString)", isDirectory: true)
    return AudioAsset(
      id: uuid(70),
      meetingID: meetingID,
      url: folder.appendingPathComponent("master.caf"),
      format: .caf48kFloat32,
      lanes: [.mic, .system],
      sidecars16k: [
        .mic: folder.appendingPathComponent("mic.wav"),
        .system: folder.appendingPathComponent("system.wav"),
      ],
      mixdownURL: folder.appendingPathComponent("audio.m4a"),
      retention: .keepDays(30),
      expiresAt: Date(timeIntervalSince1970: 1_793_009_100)
    )
  }

  public static func export() -> MeetingExport {
    MeetingExport(
      meeting: meeting(),
      participants: participants(),
      speakers: speakers(),
      persons: persons(),
      segments: segments(),
      tasks: tasks(),
      decisions: decisions(),
      audio: audioAsset()
    )
  }

  public static func delivery() -> Delivery {
    Delivery(
      id: uuid(80), meetingID: meetingID, destinationID: "obsidian-folder", status: .delivered,
      lastAttemptAt: updatedAt,
      receipt: DeliveryReceipt(
        root: "/tmp/vault", folder: "Meetings/2026-09-24-produktstrategie-90-10",
        files: [
          DeliveredFile(
            relativePath: "meeting.json", ownership: .owned,
            sha256: Data(repeating: 0xAB, count: 32))
        ],
        rendererVersion: 1))
  }

  public static func pairedDevice() -> PairedDevice {
    PairedDevice(id: uuid(90), name: "Nicolais iPhone", pairedAt: createdAt, lastSeenAt: updatedAt)
  }

  public static func handoverReceipt() -> HandoverReceipt {
    HandoverReceipt(
      recordingID: uuid(91), deviceID: uuid(90), state: .complete(meetingID: meetingID),
      byteCount: 4096, sha256: Data(repeating: 0x01, count: 32), chunkSize: 1024,
      receivedChunks: [0, 1, 2, 3], createdAt: createdAt, updatedAt: updatedAt)
  }

  public static func recordingMetadata() -> RecordingMetadata {
    RecordingMetadata(
      recordingID: uuid(91), startedAt: startedAt, durationSeconds: 6, byteCount: 4096,
      sha256: Data(repeating: 0x01, count: 32), chunkSize: 1024, format: .m4aAAC,
      deviceName: "Nicolais iPhone")
  }

  public static func summaryOutput() -> SummaryOutput {
    SummaryOutput(
      title: "Produktstrategie: 90/10",
      summary: summaryDocument(),
      decisions: decisions().map(\.text),
      tasks: tasks(),
      speakerNames: [
        SpeakerNameSuggestion(
          speakerID: speakerTwoID, name: "Jérôme", confidence: 0.8,
          evidence: "Speaker 1 spricht Speaker 2 mit Jérôme an.")
      ],
      language: Locale.Language(stenoIdentifier: "de"),
      usage: LLMUsage(promptTokens: 800, completionTokens: 200, requests: 1)
    )
  }

  public static func settings() -> Settings {
    Settings(
      audioFolder: URL(fileURLWithPath: "/tmp/steno/audio", isDirectory: true),
      defaultRetention: .keepDays(7),
      inputDeviceUID: "BuiltInMic",
      meetingDetectionEnabled: false,
      speechEngineID: "whisperkit-large-v3-turbo",
      speakerMatchThreshold: 0.65,
      modelsDirectory: URL(fileURLWithPath: "/tmp/steno/models", isDirectory: true),
      llmBaseURL: URL(string: "http://127.0.0.1:1234/v1"),
      llmModel: "local-model",
      llmContextTokens: 16_000,
      defaultTemplateID: "daily-standup",
      launchAtLogin: false,
      obsidian: ObsidianSettings(
        vaultPath: "/tmp/vault", peopleFolder: "People", includeAudio: true, taskTag: "#steno")
    )
  }

  public static func llmRequest() -> LLMRequest {
    LLMRequest(
      messages: [
        LLMMessage(role: .system, content: "You clean transcripts."),
        LLMMessage(role: .user, content: "wir setzen 90 prozent auf den kern"),
      ],
      responseFormat: .jsonSchema(
        name: "cleanup",
        schema: [
          "type": "object", "properties": ["segments": ["type": "array"]], "required": ["segments"],
          "additionalProperties": false,
        ],
        strict: true),
      temperature: 0.2,
      maxTokens: 2048,
      purpose: "cleanup"
    )
  }

  public static func llmResponse() -> LLMResponse {
    LLMResponse(
      text: "{\"segments\":[]}", finishReason: .stop,
      usage: LLMUsage(promptTokens: 10, completionTokens: 5, requests: 1), model: "local-model")
  }

  public static func rawSegments() -> [RawSegment] {
    [
      RawSegment(
        start: 0, end: 2.5, text: "wir setzen 90 prozent auf den kern",
        language: Locale.Language(stenoIdentifier: "de"),
        wordTimings: [WordTiming(word: "wir", start: 0, end: 0.3)]),
      RawSegment(start: 2.5, end: 5.5, text: "budget check on friday", language: nil),
    ]
  }

  public static func diarization() -> DiarizationResult {
    DiarizationResult(clusters: [
      SpeakerCluster(
        label: "Speaker 1", ranges: [0...2.5], embedding: embedding(axis: 0),
        clusterConfidence: 0.9,
        sampleClipRange: 0.5...2.5),
      SpeakerCluster(
        label: "Speaker 2", ranges: [2.5...5.5, 5.5...6], embedding: embedding(axis: 1),
        clusterConfidence: 0.75, sampleClipRange: 3...5.5),
    ])
  }

  public static func template() -> SummaryTemplate {
    SummaryTemplate(
      id: "default", displayName: "Default", description: "General meeting summary.",
      context: "A general meeting.",
      sections: [
        TemplateSection(
          id: "executive-summary", heading: "Executive Summary",
          instructions: "Three to five bullets.", required: true)
      ])
  }
}
