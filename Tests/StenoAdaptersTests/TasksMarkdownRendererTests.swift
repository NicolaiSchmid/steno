import Foundation
import StenoCore
import Testing

@testable import StenoAdapters

@Suite struct TasksMarkdownRendererTests {
  let renderer = ArtifactRenderer()
  let export = FixtureMeeting.export()

  @Test func matchesTheGoldens() throws {
    try Snapshot.assert(
      renderer.renderTasks(export, options: FixtureMeeting.plain),
      matches: "snapshots/obsidian/tasks-plain.md")
    try Snapshot.assert(
      renderer.renderTasks(export, options: FixtureMeeting.wikilink),
      matches: "snapshots/obsidian/tasks-wikilink.md")
    try Snapshot.assert(
      renderer.renderTasks(export, options: FixtureMeeting.wikilinkTag),
      matches: "snapshots/obsidian/tasks-wikilink-tag.md")
  }

  @Test func linesFollowTheVerifiedFieldOrder() {
    let note = renderer.renderTasks(export, options: FixtureMeeting.wikilinkTag)
    let lines = note.split(separator: "\n").filter { $0.hasPrefix("- [") }.map(String.init)
    #expect(
      lines == [
        "- [ ] Angebot an ACME schicken [[Anna Müller]] #task \u{23EB} \u{1F4C5} 2026-10-01",
        "- [ ] Roadmap-Folien aktualisieren [[Nicolai Schmid]] #task \u{1F4C5} 2026-10-15",
        "- [x] Protokoll verteilen #task \u{1F53D}",
        "- [ ] Budget freigeben lassen Jérôme Dupont #task",
      ])
    #expect(
      lines[0].contains("[[Anna Müller]]"),
      "the assignee's current person name, not the model's \"Anna\"")
    #expect(note.hasSuffix("\n\nEdit tasks in Steno; this file is rewritten on re-export.\n"))
    #expect(
      note.hasPrefix(
        "---\ntitle: \"Produktstrategie: \\\"90/10\\\" & Roadmap für Q4 — Tasks\"\ntype: \"tasks\"\n"
      ))
  }

  @Test func emojiFieldsEndTheLineWithoutVariationSelectorsOrNoBreakSpaces() throws {
    let note = renderer.renderTasks(export, options: FixtureMeeting.wikilinkTag)
    #expect(!note.unicodeScalars.contains("\u{FE0F}"))
    #expect(!note.unicodeScalars.contains("\u{00A0}"))
    let fieldTail = try Regex(
      "^- \\[[ x]\\] .*?( \\[\\[[^\\]]+\\]\\])?( #task)?( \u{23EB}| \u{1F53D})?( \u{1F4C5} \\d{4}-\\d{2}-\\d{2})?$"
    )
    for line in note.split(separator: "\n") where line.hasPrefix("- [") {
      #expect(String(line).wholeMatch(of: fieldTail) != nil, "\(line)")
    }
    let normal = note.split(separator: "\n").first { $0.contains("Roadmap-Folien") }
    #expect(
      normal?.contains("\u{23EB}") == false && normal?.contains("\u{1F53D}") == false,
      ".normal has no priority emoji")
  }

  @Test func plainStyleAndNoTasks() {
    let plain = renderer.renderTasks(export, options: FixtureMeeting.plain)
    #expect(
      plain.contains("\n- [ ] Angebot an ACME schicken Anna Müller \u{23EB} \u{1F4C5} 2026-10-01\n")
    )
    #expect(!plain.contains("#task"))
    var export = self.export
    export.tasks = []
    let empty = renderer.renderTasks(export, options: FixtureMeeting.plain)
    #expect(
      empty.hasSuffix(
        "— Tasks\n\nNo tasks.\n\nEdit tasks in Steno; this file is rewritten on re-export.\n"))
  }

  @Test func anAssigneeNamedByClusterLabelResolvesToThePerson() {
    var export = self.export
    export.tasks = [
      MeetingTask(
        id: SampleData.uuid(60), meetingID: export.meeting.id, text: "Nachfassen",
        assigneeName: "Speaker 1"),
      MeetingTask(
        id: SampleData.uuid(61), meetingID: export.meeting.id, text: "Offen lassen",
        assigneeName: "Speaker 2"),
    ]
    let note = renderer.renderTasks(export, options: FixtureMeeting.wikilink)
    #expect(note.contains("\n- [ ] Nachfassen [[Anna Müller]]\n"), "Speaker 1 is Anna")
    #expect(note.contains("\n- [ ] Offen lassen Speaker 2\n"), "Speaker 2 resolved to nobody")
  }

  @Test func theTagIsSanitised() {
    let options = RenderOptions(linkStyle: .wikilink, personPages: true, taskTag: "#steno tasks")
    let note = renderer.renderTasks(export, options: options)
    #expect(note.contains(" #steno-tasks "))
  }

  @Test func everyPriorityOnABareTaskAndTheDoneMarker() {
    var export = self.export
    export.tasks = TaskPriority.allCases.enumerated().map { index, priority in
      MeetingTask(
        id: SampleData.uuid(80 + index), meetingID: export.meeting.id, text: "Nur \(priority)",
        priority: priority)
    }
    export.tasks.append(
      MeetingTask(
        id: SampleData.uuid(90), meetingID: export.meeting.id, text: "Erledigt", priority: .high,
        dueDate: FixtureMeeting.dueOctoberFirst, done: true))
    let note = renderer.renderTasks(export, options: FixtureMeeting.wikilinkTag)
    let lines = note.split(separator: "\n").filter { $0.hasPrefix("- [") }.map(String.init)
    #expect(
      lines == [
        "- [ ] Nur low #task \u{1F53D}",
        "- [ ] Nur normal #task",
        "- [ ] Nur high #task \u{23EB}",
        "- [x] Erledigt #task \u{23EB} \u{1F4C5} 2026-10-01",
      ], "no assignee, no due date: the line ends after the tag or the priority")
    let plain = renderer.renderTasks(export, options: .plain)
    #expect(plain.contains("\n- [ ] Nur normal\n"), "a bare normal task is text alone")
    #expect(!plain.contains("\u{2705}"), "done writes [x] and no completion date")
  }

  @Test func aDueDateFollowsTheOptionsTimeZone() {
    var export = self.export
    // 2026-10-01T23:30:00Z: still the 1st in UTC, already the 2nd in Berlin.
    export.tasks = [
      MeetingTask(
        id: SampleData.uuid(81), meetingID: export.meeting.id, text: "Spät",
        dueDate: Date(timeIntervalSince1970: 1_790_897_400))
    ]
    #expect(
      renderer.renderTasks(export, options: .plain).contains("\n- [ ] Spät \u{1F4C5} 2026-10-01\n"))
    #expect(
      renderer.renderTasks(export, options: FixtureMeeting.plainBerlin).contains(
        "\n- [ ] Spät \u{1F4C5} 2026-10-02\n"))
  }
}
