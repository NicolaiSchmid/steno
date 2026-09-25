import Foundation
import StenoCore

/// Synthetic transcripts for the LLM tests and the CLI, as `MeetingExport`
/// values: a 24-segment Denglish standup with deliberate STT errors, and a
/// generated 60-minute customer call of about 900 segments that forces map
/// and reduce at an 8k budget. Sequential ids, fixed dates, invented text;
/// `Tests/Fixtures/llm/transcripts/*.json` hold the same values and
/// `LLMFixturesTests` keeps them equal.
public enum LLMFixtures {
  /// `SampleData.persons()` without embeddings, which `meeting.json` never
  /// carries, so a fixture round-trips through JSON as an equal value.
  static var knownPeople: [Person] {
    SampleData.persons().map { person in
      var stripped = person
      stripped.embedding = nil
      stripped.sampleCount = 0
      return stripped
    }
  }

  // MARK: Denglish standup

  public static let standupMeetingID = SampleData.uuid(100)
  public static let standupSpeakerOneID = SampleData.uuid(110)
  public static let standupSpeakerTwoID = SampleData.uuid(111)
  public static let standupSpeakerThreeID = SampleData.uuid(112)
  public static let personMaraID = SampleData.uuid(12)

  /// Three colleagues, German with English tech vocabulary, lowercase STT
  /// output with the usual misspellings ("git hub", "kuber netes",
  /// "jerome"). Speaker 1 leads, Speaker 2 is Nicolai (addressed by name in
  /// the first line, unconfirmed in the fixture), Speaker 3 is Jérôme.
  public static func denglishStandup() -> MeetingExport {
    let meetingID = standupMeetingID
    let lines: [(Int, String)] = [
      (1, "okay lass uns anfangen. nicolai, willst du starten?"),
      (
        2,
        "klar. gestern habe ich den pull request für das onboarding gemerged und das deployment auf staging gepusht."
      ),
      (
        2,
        "heute schaue ich mir die flaky tests in der ci pipeline an, die laufen seit dem git hub upgrade nicht mehr stabil."
      ),
      (1, "hast du einen blocker?"),
      (2, "nein, aber ich brauche von jerome nochmal die zugangsdaten für das test environment."),
      (
        3,
        "kann ich dir nach dem standup schicken. ich habe gestern das kuber netes cluster auf die neue version gezogen."
      ),
      (
        3,
        "heute mache ich die dokumentation für die api endpoints fertig und reviewe dein pull request, nicolai."
      ),
      (1, "super. gibt es was von der kundenseite?"),
      (3, "die firma müller hat gefragt ob wir den export nach obsidian bis freitag schaffen."),
      (1, "das ist high priority, ich würde sagen wir committen uns auf freitag."),
      (2, "okay, dann nehme ich das mit rein, das ist ein kleiner change im markdown renderer."),
      (1, "ich selber habe gestern die road map für q4 mit dem product owner durchgesprochen."),
      (1, "heute bereite ich das sprint review vor und update die tickets in linear."),
      (3, "ein blocker bei mir: der mac book pro von der it kommt erst nächste woche."),
      (1, "okay, ich frage nochmal nach. noch was?"),
      (2, "eine frage: sollen wir die retro auf donnerstag verschieben?"),
      (1, "ja lass uns das machen, donnerstag vierzehn uhr."),
      (3, "passt für mich."),
      (2, "passt."),
      (
        1,
        "dann noch eine announcement: ab nächster woche haben wir eine neue kollegin im team, sie heißt lena."
      ),
      (3, "cool. soll ich ein onboarding dokument vorbereiten?"),
      (1, "ja bitte, bis mittwoch wäre gut."),
      (3, "mache ich."),
      (1, "danke euch, das wars für heute."),
    ]
    let speakerIDs = [1: standupSpeakerOneID, 2: standupSpeakerTwoID, 3: standupSpeakerThreeID]
    var segments: [TranscriptSegment] = []
    var time: TimeInterval = 0
    for (index, line) in lines.enumerated() {
      let words = line.1.split(separator: " ").count
      let duration = TimeInterval(words) * 0.5 + 0.5
      segments.append(
        TranscriptSegment(
          id: UUID(derivedFrom: meetingID, salt: "segment-mixed-\(index)"),
          meetingID: meetingID, start: time, end: time + duration,
          speakerID: speakerIDs[line.0], lane: .mixed, text: line.1, rawText: line.1))
      time += duration
    }
    let meeting = Meeting(
      id: meetingID, title: "Daily Standup", startedAt: SampleData.startedAt,
      duration: time.rounded(.up), language: "de", source: .macInPerson,
      calendarEventID: "standup-2026-09-24", state: .ready, templateID: "daily-standup",
      createdAt: SampleData.createdAt, updatedAt: SampleData.updatedAt)
    let persons =
      knownPeople + [
        Person(id: personMaraID, displayName: "Mara", createdAt: SampleData.createdAt)
      ]
    let participants = [
      Participant(
        id: SampleData.uuid(130), meetingID: meetingID, personID: personMaraID,
        displayName: "Mara", role: .them),
      Participant(
        id: SampleData.uuid(131), meetingID: meetingID, personID: SampleData.personJeromeID,
        displayName: "Jérôme", role: .them),
      Participant(
        id: SampleData.uuid(132), meetingID: meetingID, personID: SampleData.personNicolaiID,
        displayName: "Nicolai", role: .me, email: "nicolai@example.com"),
    ]
    let speakers = [
      Speaker(
        id: standupSpeakerOneID, meetingID: meetingID, clusterLabel: "Speaker 1",
        assignment: .suggested(personID: personMaraID, similarity: 0.7), clusterConfidence: 0.8),
      Speaker(
        id: standupSpeakerTwoID, meetingID: meetingID, clusterLabel: "Speaker 2",
        clusterConfidence: 0.85),
      Speaker(
        id: standupSpeakerThreeID, meetingID: meetingID, clusterLabel: "Speaker 3",
        assignment: .confirmed(personID: SampleData.personJeromeID), clusterConfidence: 0.9),
    ]
    return MeetingExport(
      meeting: meeting, participants: participants, speakers: speakers, persons: persons,
      segments: segments, tasks: [], decisions: [], audio: nil)
  }

  // MARK: 60-minute customer call

  public static let callMeetingID = SampleData.uuid(200)
  public static let callSpeakerMeID = SampleData.uuid(210)
  public static let callSpeakerOneID = SampleData.uuid(211)
  public static let callSpeakerTwoID = SampleData.uuid(212)
  public static let personPetraID = SampleData.uuid(13)
  public static let personTomID = SampleData.uuid(14)

  /// A Mac call of exactly one hour: `Me` (Nicolai, the vendor) on the mic
  /// lane, `Speaker 1` and `Speaker 2` (a logistics customer) on the system
  /// lane, 900 segments of four seconds, text drawn from phrase banks by a
  /// seeded generator.
  public static func customerCall60min() -> MeetingExport {
    let meetingID = callMeetingID
    let generated = SyntheticTranscript.generate(
      meetingID: meetingID, seed: 0x5EED_2026_0925,
      speakers: [
        .init(id: callSpeakerMeID, lane: .mic, role: .vendor, weight: 4),
        .init(id: callSpeakerOneID, lane: .system, role: .customer, weight: 4),
        .init(id: callSpeakerTwoID, lane: .system, role: .colleague, weight: 2),
      ],
      segmentCount: 900, segmentSeconds: 4)
    let meeting = Meeting(
      id: meetingID, title: "Discovery call Nordkette Logistik", startedAt: SampleData.startedAt,
      duration: 3_600, language: "de", source: .macCall, state: .ready,
      templateID: "customer-discovery", createdAt: SampleData.createdAt,
      updatedAt: SampleData.updatedAt)
    let persons =
      knownPeople + [
        Person(id: personPetraID, displayName: "Petra Vogel", createdAt: SampleData.createdAt),
        Person(id: personTomID, displayName: "Tom Berger", createdAt: SampleData.createdAt),
      ]
    let participants = [
      Participant(
        id: SampleData.uuid(230), meetingID: meetingID, personID: SampleData.personNicolaiID,
        displayName: "Nicolai", role: .me, email: "nicolai@example.com"),
      Participant(
        id: SampleData.uuid(231), meetingID: meetingID, displayName: "Petra Vogel", role: .them,
        email: "petra.vogel@example.com"),
      Participant(
        id: SampleData.uuid(232), meetingID: meetingID, displayName: "Tom Berger", role: .them),
    ]
    let speakers = [
      Speaker(
        id: callSpeakerMeID, meetingID: meetingID, clusterLabel: "Me",
        assignment: .confirmed(personID: SampleData.personNicolaiID), clusterConfidence: 1),
      Speaker(
        id: callSpeakerOneID, meetingID: meetingID, clusterLabel: "Speaker 1",
        clusterConfidence: 0.8),
      Speaker(
        id: callSpeakerTwoID, meetingID: meetingID, clusterLabel: "Speaker 2",
        clusterConfidence: 0.7),
    ]
    return MeetingExport(
      meeting: meeting, participants: participants, speakers: speakers, persons: persons,
      segments: generated, tasks: [], decisions: [], audio: nil)
  }
}

/// A seeded transcript generator: SplitMix64, integer arithmetic only, so
/// the output is byte-identical on every machine.
enum SyntheticTranscript {
  enum Role: Sendable {
    case vendor, customer, colleague
  }

  struct SpeakerSpec: Sendable {
    var id: UUID
    var lane: AudioLane
    var role: Role
    /// Relative share of turns.
    var weight: Int
  }

  struct SplitMix64 {
    var state: UInt64

    mutating func next() -> UInt64 {
      state &+= 0x9E37_79B9_7F4A_7C15
      var z = state
      z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
      z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
      return z ^ (z >> 31)
    }

    mutating func below(_ bound: Int) -> Int {
      Int(next() % UInt64(max(bound, 1)))
    }
  }

  static func generate(
    meetingID: UUID, seed: UInt64, speakers: [SpeakerSpec], segmentCount: Int,
    segmentSeconds: TimeInterval
  ) -> [TranscriptSegment] {
    var rng = SplitMix64(state: seed)
    var segments: [TranscriptSegment] = []
    var speakerIndex = 0
    var remainingTurn = 0
    let totalWeight = speakers.reduce(0) { $0 + $1.weight }
    for index in 0..<segmentCount {
      if remainingTurn == 0 {
        var pick = rng.below(totalWeight)
        for (offset, speaker) in speakers.enumerated() {
          if pick < speaker.weight {
            speakerIndex = offset
            break
          }
          pick -= speaker.weight
        }
        remainingTurn = 1 + rng.below(4)
      }
      remainingTurn -= 1
      let speaker = speakers[speakerIndex]
      let text = sentence(for: speaker.role, rng: &rng)
      let start = TimeInterval(index) * segmentSeconds
      segments.append(
        TranscriptSegment(
          id: UUID(derivedFrom: meetingID, salt: "segment-\(speaker.lane.rawValue)-\(index)"),
          meetingID: meetingID, start: start, end: start + segmentSeconds - 0.25,
          speakerID: speaker.id, lane: speaker.lane, text: text, rawText: text))
    }
    return segments
  }

  static func sentence(for role: Role, rng: inout SplitMix64) -> String {
    let bank: [String]
    switch role {
    case .vendor: bank = vendorLines
    case .customer: bank = customerLines
    case .colleague: bank = colleagueLines
    }
    // Half of all turns are short acknowledgements, as in a real call.
    if rng.below(2) == 0 {
      return shortLines[rng.below(shortLines.count)]
    }
    var text = bank[rng.below(bank.count)]
    if rng.below(4) == 0 {
      text += " " + fillers[rng.below(fillers.count)]
    }
    return text
  }

  static let shortLines = [
    "ja genau.", "das stimmt.", "okay, verstehe.", "mhm.", "right, okay.", "gut, dann so.",
    "kurze rückfrage dazu.", "das wäre wichtig.", "ja, das kenne ich.", "moment, ich notiere.",
    "okay.", "verstehe ich.", "ja.", "exactly.", "das passt.", "warte kurz.", "ah okay.",
    "gute frage.", "das klingt gut.", "einen moment bitte.",
  ]

  static let fillers = [
    "genau.", "okay.", "right.", "also ja.", "verstehe.", "makes sense.", "gut.", "mhm.",
  ]

  static let vendorLines = [
    "danke dass ihr euch die zeit nehmt, ich würde gerne erst mal verstehen wie ihr heute plant.",
    "wie läuft die disposition bei euch aktuell ab, eher excel oder ein eigenes tool?",
    "das heißt die fahrer bekommen ihre touren am abend vorher per whatsapp?",
    "verstehe, und wie oft ändert sich so eine tour am tag noch mal?",
    "wir haben das bei einem anderen kunden so gelöst dass die umplanung automatisch läuft.",
    "die integration in euer erp wäre über die rest api möglich, das dauert meist zwei wochen.",
    "ich zeige euch kurz das dashboard, hier seht ihr alle touren in real time.",
    "the pricing depends on the number of vehicles, not on the number of users.",
    "wenn ihr wollt setzen wir einen pilot mit einem depot auf, drei monate, ohne commitment.",
    "was wäre für euch der wichtigste erfolgsfaktor in so einem pilot?",
    "das mit dem datenschutz klären wir mit unserem dpa, die daten bleiben in frankfurt.",
    "ich schicke euch nach dem call eine zusammenfassung und den vorschlag für den pilot.",
    "wer wäre bei euch der ansprechpartner für die technische anbindung?",
    "gute frage, das offline szenario haben wir gelöst, die app cached die tour lokal.",
    "let me make a note of that, that is a really good point about the drivers.",
    "wie viele fahrzeuge sind das insgesamt über alle standorte?",
    "okay, dann würde ich vorschlagen wir machen nächste woche einen technischen deep dive.",
    "das support modell ist business hours per default, 24 7 ist optional.",
    "die schulung für die disponenten dauert einen halben tag, das machen wir vor ort.",
    "kann ich fragen was euch am aktuellen prozess am meisten stört?",
  ]

  static let customerLines = [
    "also wir planen aktuell mit excel, jeder disponent hat seine eigene datei.",
    "die fahrer bekommen die tour am vorabend, änderungen kommen dann per telefon.",
    "das problem ist dass wir bei kurzfristigen ausfällen komplett blind sind.",
    "wir haben etwa achtzig fahrzeuge an drei standorten, hauptsächlich sprinter.",
    "unser erp ist ein älteres system, die schnittstelle ist ehrlich gesagt nicht toll.",
    "das dashboard sieht gut aus, aber wie sieht das auf dem handy vom fahrer aus?",
    "die fahrer sind teilweise nicht so technikaffin, das muss wirklich simpel sein.",
    "budget haben wir für dieses jahr noch, aber die entscheidung trifft die geschäftsführung.",
    "ein pilot mit dem depot in hannover wäre denkbar, da ist der leidensdruck am größten.",
    "wichtig wäre uns dass die daten nicht irgendwo in den usa liegen.",
    "wie schnell könnte so ein pilot starten, realistisch?",
    "wir hatten schon mal ein tool getestet, das ist an der akzeptanz der fahrer gescheitert.",
    "was passiert wenn der fahrer im funkloch ist, geht die app dann noch?",
    "die umplanung am tag ist bei uns eher die regel als die ausnahme.",
    "ich müsste tom noch dazu holen, er kennt die it seite besser als ich.",
    "können wir die fahrer selbst in dem tool anlegen oder macht ihr das?",
    "das mit der schulung vor ort ist gut, unsere disponenten sitzen alle in hannover.",
    "ehrlich gesagt ist der größte schmerz das telefonieren am morgen.",
    "schickt mir bitte die unterlagen, ich bespreche das dann intern.",
    "wir würden das gerne bis ende oktober entscheiden.",
  ]

  static let colleagueLines = [
    "von der it seite ist die frage wie die authentifizierung läuft, wir haben azure ad.",
    "die rest api klingt gut, habt ihr eine dokumentation die ich mir anschauen kann?",
    "wir müssten die stammdaten aus dem erp einmal am tag synchronisieren.",
    "das offline thema ist für uns kritisch, im harz gibt es kaum netz.",
    "wie sieht es mit dem export aus, wir brauchen die touren am ende in unserem bi tool.",
    "ich würde mir das gerne mal in einer test umgebung anschauen.",
    "gibt es ein sla für die api, und was ist die uptime der letzten zwölf monate?",
    "die handys der fahrer sind android, teilweise ältere modelle.",
    "kann ich einen technischen ansprechpartner bei euch bekommen?",
    "das klingt machbar, den deep dive nächste woche würde ich übernehmen.",
  ]
}
