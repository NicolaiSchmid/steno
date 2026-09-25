import Foundation
import StenoAdapters
import StenoCore

/// The synthetic meeting every adapter golden is rendered from. Sequential
/// UUIDs, fixed dates, invented text; `Tests/Fixtures/meetings/
/// produktstrategie.json` is its `StenoJSON` encoding and
/// `MeetingJSONRendererTests` keeps the two in step.
enum FixtureMeeting {
  static let meetingID = SampleData.uuid(1)
  static let annaID = SampleData.uuid(10)
  static let nicolaiID = SampleData.uuid(11)
  static let speakerMeID = SampleData.uuid(20)
  static let speakerOneID = SampleData.uuid(21)
  static let speakerTwoID = SampleData.uuid(22)

  /// 2026-09-24T12:00:00Z, 14:00 in Berlin.
  static let startedAt = Date(timeIntervalSince1970: 1_790_251_200)
  static let createdAt = Date(timeIntervalSince1970: 1_790_256_600)
  static let updatedAt = Date(timeIntervalSince1970: 1_790_257_200)
  /// 2026-10-01T12:00:00Z and 2026-10-15T12:00:00Z: noon, so the day is the
  /// same in Berlin and UTC.
  static let dueOctoberFirst = Date(timeIntervalSince1970: 1_790_856_000)
  static let dueOctoberFifteenth = Date(timeIntervalSince1970: 1_792_065_600)

  static let title = "Produktstrategie: \"90/10\" & Roadmap für Q4"
  static let folderSlug = "2026-09-24-produktstrategie-90-10-roadmap-fuer-q4"
  static let folder = "Meetings/\(folderSlug)"

  static let berlin = TimeZone(identifier: "Europe/Berlin")!

  /// Plain names, no people, no tag, UTC: the `.plain` preset.
  static let plain = RenderOptions.plain
  static let plainBerlin = RenderOptions(timeZone: berlin)
  /// What the Obsidian destination uses with a people folder, in Berlin.
  static let wikilink = RenderOptions(linkStyle: .wikilink, personPages: true, timeZone: berlin)
  static let wikilinkUTC = RenderOptions(linkStyle: .wikilink, personPages: true)
  static let wikilinkTag = RenderOptions(
    linkStyle: .wikilink, personPages: true, taskTag: "task", timeZone: berlin)

  static func export() -> MeetingExport {
    MeetingExport(
      meeting: meeting(),
      participants: participants(),
      speakers: speakers(),
      persons: persons(),
      segments: segments(),
      tasks: tasks(),
      decisions: decisions(),
      audio: audio())
  }

  static func meeting() -> Meeting {
    Meeting(
      id: meetingID,
      title: title,
      startedAt: startedAt,
      duration: 5400,
      language: "de",
      source: .macCall,
      calendarEventID: "event-42",
      tags: ["Kunde ACME", "#q4"],
      state: .ready,
      templateID: SummaryTemplate.defaultID,
      summary: SummaryDocument(
        templateID: SummaryTemplate.defaultID,
        language: "de",
        sections: [
          SummarySection(
            id: "executive-summary", heading: "Executive Summary",
            bullets: [
              SummaryBullet(
                lead: "Fokus",
                text: "Speaker 1 setzt 90 Prozent auf den Kern und 10 auf Experimente."),
              SummaryBullet(
                lead: "Protokoll", text: "Speaker 2 verteilt das Protokoll nach dem Termin."),
            ]),
          SummarySection(
            id: "next-steps", heading: "Nächste Schritte",
            bullets: [
              SummaryBullet(
                lead: "", text: "Angebot an ACME bis 1. Oktober; Roadmap-Folien bis Mitte Oktober.")
            ]),
        ]),
      scratchpad:
        "Nachfassen wegen Budget.\n\n---\n\n## Summary\n\nNicht vergessen: Jérôme fragen.",
      llmUsage: LLMUsage(promptTokens: 4200, completionTokens: 900, requests: 5),
      createdAt: createdAt,
      updatedAt: updatedAt)
  }

  /// Ordered by display name, as `MeetingStore.export` returns them.
  static func participants() -> [Participant] {
    [
      Participant(
        id: SampleData.uuid(30), meetingID: meetingID, personID: annaID, displayName: "Anna Müller",
        role: .them, email: "anna@example.com"),
      Participant(
        id: SampleData.uuid(32), meetingID: meetingID, displayName: "Jérôme Dupont", role: .them),
      Participant(
        id: SampleData.uuid(31), meetingID: meetingID, personID: nicolaiID,
        displayName: "Nicolai Schmid", role: .me, email: "nicolai@example.com"),
    ]
  }

  /// Ordered by display name.
  static func persons() -> [Person] {
    [
      Person(
        id: annaID, displayName: "Anna Müller", email: "anna@example.com", sampleCount: 4,
        createdAt: createdAt),
      Person(
        id: nicolaiID, displayName: "Nicolai Schmid", email: "nicolai@example.com", sampleCount: 9,
        createdAt: createdAt),
    ]
  }

  /// Ordered by cluster label.
  static func speakers() -> [Speaker] {
    [
      Speaker(
        id: speakerMeID, meetingID: meetingID, clusterLabel: "Me",
        assignment: .confirmed(personID: nicolaiID), clusterConfidence: 1),
      Speaker(
        id: speakerOneID, meetingID: meetingID, clusterLabel: "Speaker 1",
        assignment: .confirmed(personID: annaID), sampleClipRange: 4.5...14.5,
        clusterConfidence: 0.88),
      Speaker(
        id: speakerTwoID, meetingID: meetingID, clusterLabel: "Speaker 2", assignment: .unknown,
        sampleClipRange: 30.5...36,
        sampleClipURL: URL(
          fileURLWithPath:
            "/tmp/steno/\(meetingID.uuidString)/speakers/\(speakerTwoID.uuidString).wav"),
        clusterConfidence: 0.7),
    ]
  }

  /// Fourteen segments over both lanes: consecutive turns, a gap of more
  /// than three seconds inside a turn, one zero-length segment, one without
  /// a speaker, and texts that start with `#`, `-`, `>` and `1.` or carry
  /// `<`, `&` and `-->`.
  static func segments() -> [TranscriptSegment] {
    func segment(
      _ n: Int, _ start: TimeInterval, _ end: TimeInterval, _ speaker: UUID?, _ lane: AudioLane,
      _ text: String
    ) -> TranscriptSegment {
      TranscriptSegment(
        id: SampleData.uuid(40 + n), meetingID: meetingID, start: start, end: end,
        speakerID: speaker, lane: lane, text: text, rawText: text.lowercased())
    }
    return [
      segment(
        0, 0, 4.2, speakerMeID, .mic, "Guten Morgen zusammen, fangen wir mit der Roadmap an."),
      segment(1, 4.5, 9.8, speakerOneID, .system, "Gern. Ich habe die Zahlen für Q4 dabei."),
      segment(2, 10, 15.2, speakerOneID, .system, "Der Kern bekommt 90 Prozent, der Rest 10."),
      segment(3, 19, 24, speakerOneID, .system, "Das ACME-Angebot muss bis zum 1. Oktober raus."),
      segment(4, 24.5, 30, speakerMeID, .mic, "# Punkt eins: Budget <Kern> & Rest --> offen"),
      segment(5, 30.5, 36, speakerTwoID, .system, "Ich kann das Protokoll verteilen."),
      segment(6, 36, 36, speakerTwoID, .system, "Okay."),
      segment(7, 37, 39.5, nil, .system, "- Notiz: Einwurf aus dem Off."),
      segment(8, 40, 46, speakerMeID, .mic, "1. Wir starten im Oktober."),
      segment(9, 46.5, 52, speakerOneID, .system, "> Zitat aus dem Kundenbrief."),
      segment(10, 52.5, 58, speakerMeID, .mic, "Jérôme, kannst du das Budget freigeben lassen?"),
      segment(11, 60, 66, speakerTwoID, .system, "Ja, ich kümmere mich darum."),
      segment(12, 66.5, 70, speakerOneID, .system, "Dann sind wir durch."),
      segment(13, 70.5, 72, speakerMeID, .mic, "Danke, bis nächste Woche."),
    ]
  }

  /// Every priority, with and without due date and assignee, one done, one
  /// with a line break in its text.
  static func tasks() -> [MeetingTask] {
    [
      MeetingTask(
        id: SampleData.uuid(50), meetingID: meetingID, text: "Angebot an ACME schicken",
        assigneePersonID: annaID, assigneeName: "Anna", priority: .high, dueDate: dueOctoberFirst),
      MeetingTask(
        id: SampleData.uuid(51), meetingID: meetingID, text: "Roadmap-Folien aktualisieren",
        assigneePersonID: nicolaiID, assigneeName: "Nicolai Schmid", priority: .normal,
        dueDate: dueOctoberFifteenth),
      MeetingTask(
        id: SampleData.uuid(52), meetingID: meetingID, text: "Protokoll verteilen", priority: .low,
        done: true),
      MeetingTask(
        id: SampleData.uuid(53), meetingID: meetingID, text: "Budget\nfreigeben lassen",
        assigneeName: "Jérôme Dupont", priority: .normal),
    ]
  }

  static func decisions() -> [Decision] {
    ["90/10-Aufteilung wird umgesetzt.", "Roadmap-Review am 15. Oktober."].enumerated().map {
      index, text in
      Decision(
        id: UUID(derivedFrom: meetingID, salt: "decision-\(index)"), meetingID: meetingID,
        text: text)
    }
  }

  static func audio() -> AudioAsset {
    let folder = URL(fileURLWithPath: "/tmp/steno/\(meetingID.uuidString)", isDirectory: true)
    return AudioAsset(
      id: SampleData.uuid(70),
      meetingID: meetingID,
      url: folder.appendingPathComponent("recording.caf"),
      format: .caf48kFloat32,
      lanes: [.mic, .system],
      sidecars16k: [
        .mic: folder.appendingPathComponent("mic.wav"),
        .system: folder.appendingPathComponent("system.wav"),
      ],
      mixdownURL: folder.appendingPathComponent("audio.m4a"),
      retention: .keepDays(30),
      expiresAt: Date(timeIntervalSince1970: 1_792_849_200))
  }

  /// The export with its mixdown pointing at a fresh 100-byte file under
  /// `directory`, for delivery tests that copy audio.
  static func export(audioIn directory: URL) throws -> MeetingExport {
    var export = export()
    let mixdown = directory.appendingPathComponent("audio.m4a")
    try Data((0..<100).map { UInt8($0) }).write(to: mixdown)
    export.audio?.mixdownURL = mixdown
    return export
  }
}
