import Foundation
import Testing

@testable import StenoCore

@Suite struct SpeakerOptionsTests {
  typealias Option = SpeakerOptions.Option

  static let anna = Person(
    id: SampleData.uuid(12), displayName: "Anna Berg", createdAt: SampleData.createdAt)
  static let philipp = Person(
    id: SampleData.uuid(13), displayName: "Philipp Schröder", createdAt: SampleData.createdAt)

  /// SampleData's people plus Anna and Philipp, name order.
  static let persons = SampleData.persons() + [anna, philipp]
  static let speakers = SampleData.speakers()
  static let speakerOne = speakers[0]  // confirmed Nicolai
  static let speakerTwo = speakers[1]  // suggested Jérôme
  static let jerome = SampleData.persons()[0]
  static let nicolai = SampleData.persons()[1]

  static func build(
    speaker: Speaker = speakerTwo,
    speakers: [Speaker] = speakers,
    persons: [Person] = persons,
    participants: [Participant] = SampleData.participants(),
    suggestion: SpeakerNameSuggestion? = nil,
    recent: [Person] = [],
    rejected: Set<UUID> = [],
    query: String = ""
  ) -> [Option] {
    SpeakerOptions.build(
      speaker: speaker, speakers: speakers, persons: persons, participants: participants,
      suggestion: suggestion, recent: recent, rejected: rejected, query: query)
  }

  static func suggestion(_ name: String?) -> SpeakerNameSuggestion {
    SpeakerNameSuggestion(
      speakerID: speakerTwo.id, name: name, confidence: 0.8, evidence: "Speaker 1 says the name.")
  }

  @Test func orderIsVoiceMatchGuessInviteesThisMeetingThenRecent() {
    let attendee = Participant(
      id: SampleData.uuid(32), meetingID: SampleData.meetingID, personID: Self.anna.id,
      displayName: "Anna Berg", role: .them)
    let options = Self.build(
      participants: SampleData.participants() + [attendee],
      suggestion: Self.suggestion("Ben"),
      recent: [Self.jerome, Self.nicolai, Self.philipp])
    #expect(
      options == [
        Option(kind: .person(Self.jerome), tag: .voice(.likely)),
        Option(kind: .create("Ben"), tag: .mentioned),
        Option(kind: .person(Self.anna), tag: .invited),
        Option(kind: .person(Self.nicolai), tag: .inThisMeeting),
        Option(kind: .person(Self.philipp)),
      ])
  }

  @Test func theSuggestedPersonIsNotRepeatedUnderRecent() {
    let options = Self.build(recent: [Self.jerome, Self.philipp])
    #expect(options.filter { $0.id == Self.jerome.id.uuidString }.count == 1)
    #expect(options.first == Option(kind: .person(Self.jerome), tag: .voice(.likely)))
    #expect(options.last == Option(kind: .person(Self.philipp)))
  }

  @Test func aPersonOwningAnotherSpeakerHereIsListedOnceAsInThisMeeting() {
    let options = Self.build(recent: [Self.nicolai])
    let nicolai = options.filter { $0.id == Self.nicolai.id.uuidString }
    #expect(nicolai.count == 1)
    #expect(nicolai.first?.tag == .inThisMeeting)
  }

  @Test func theSpeakersOwnPersonIsExcluded() {
    let options = Self.build(speaker: Self.speakerOne, recent: [Self.nicolai, Self.jerome])
    #expect(!options.contains { $0.id == Self.nicolai.id.uuidString })
    #expect(options == [Option(kind: .person(Self.jerome), tag: .inThisMeeting)])
  }

  @Test func theAttendeeAlreadyListedByNameIsSkipped() {
    // SampleData's "Jérôme" participant has no personID; the voice match
    // already lists Jérôme, so no create row appears.
    let options = Self.build()
    #expect(options.filter { Person.namesMatch($0.displayName, "Jérôme") }.count == 1)
    #expect(!options.contains { if case .create = $0.kind { true } else { false } })
  }

  @Test func anUnknownAttendeeBecomesACreateRow() {
    let attendee = Participant(
      id: SampleData.uuid(33), meetingID: SampleData.meetingID, displayName: "Mia Kurz",
      role: .them)
    let options = Self.build(participants: [attendee])
    #expect(options.contains(Option(kind: .create("Mia Kurz"), tag: .lastInvitee)))
    #expect(options.first { $0.id == "create:Mia Kurz" } != nil)
  }

  @Test func aBlankAttendeeNameAddsNothing() {
    let blank = Participant(
      id: SampleData.uuid(36), meetingID: SampleData.meetingID, displayName: "  ", role: .them)
    let padded = Participant(
      id: SampleData.uuid(37), meetingID: SampleData.meetingID, displayName: " Mia Kurz ",
      role: .them)
    let options = Self.build(participants: [blank, padded])
    #expect(!options.contains { $0.displayName.isEmpty })
    #expect(options.contains(Option(kind: .create("Mia Kurz"), tag: .lastInvitee)))
  }

  @Test func anAttendeeWithoutAPersonIDResolvesByName() {
    // A calendar attendee "philipp schroder" is offered as the existing
    // person Philipp Schröder, never as a create row beside them.
    let attendee = Participant(
      id: SampleData.uuid(35), meetingID: SampleData.meetingID, displayName: "philipp schroder",
      role: .them)
    let options = Self.build(participants: [attendee])
    #expect(options.contains(Option(kind: .person(Self.philipp), tag: .lastInvitee)))
    #expect(!options.contains { if case .create = $0.kind { true } else { false } })
  }

  @Test func anAttendeeAssignedToAnotherSpeakerIsNotAnAttendeeOption() {
    let attendee = Participant(
      id: SampleData.uuid(34), meetingID: SampleData.meetingID, personID: Self.nicolai.id,
      displayName: "Nicolai", role: .them)
    let options = Self.build(participants: [attendee])
    let nicolai = options.filter { $0.id == Self.nicolai.id.uuidString }
    #expect(nicolai.map(\.tag) == [.inThisMeeting])
  }

  @Test func jeromeFindsJérôme() {
    let options = Self.build(speaker: Self.speakerOne, query: "jerome")
    #expect(options == [Option(kind: .person(Self.jerome), tag: .inThisMeeting)])
  }

  @Test func theQueryFiltersEveryOption() {
    let options = Self.build(recent: [Self.philipp], query: "Nic")
    #expect(
      options == [
        Option(kind: .person(Self.nicolai), tag: .inThisMeeting),
        Option(kind: .create("Nic")),
      ])
  }

  @Test func createIsHiddenForAMatchingName() {
    let options = Self.build(speaker: Self.speakerOne, query: " JÉRÔME ")
    #expect(!options.contains { if case .create = $0.kind { true } else { false } })
    #expect(options.count == 1)
  }

  @Test func createIsHiddenForWhitespace() {
    let blank = Self.build(query: "   \n")
    let empty = Self.build(query: "")
    #expect(blank == empty)
    #expect(!blank.contains { if case .create = $0.kind { true } else { false } })
  }

  @Test func createUsesTheTrimmedQuery() {
    let options = Self.build(query: "  Anna Neu ")
    #expect(options.last == Option(kind: .create("Anna Neu")))
    #expect(options.last?.id == "create:Anna Neu")
    #expect(options.last?.displayName == "Anna Neu")
  }

  @Test func theLLMGuessIsAPersonWhenOneExists() {
    var speaker = Self.speakerTwo
    speaker.assignment = .unknown
    // Without invitees: the sample's only one is Jérôme, whom being the
    // last invitee left would tag more strongly.
    let options = Self.build(
      speaker: speaker, participants: [], suggestion: Self.suggestion("jerome"))
    #expect(
      options == [
        Option(kind: .person(Self.jerome), tag: .mentioned),
        Option(kind: .person(Self.nicolai), tag: .inThisMeeting),
      ])
    #expect(
      Self.build(speaker: speaker, suggestion: Self.suggestion("jerome")).first
        == Option(kind: .person(Self.jerome), tag: .lastInvitee))
  }

  @Test func theLLMGuessIsACreateRowWhenNobodyHasTheName() {
    let options = Self.build(suggestion: Self.suggestion(" Ben "))
    #expect(options[1] == Option(kind: .create("Ben"), tag: .mentioned))
    #expect(options[1].id == "create:Ben")
  }

  @Test func theVoiceMatchWinsOverTheSameLLMGuess() {
    let options = Self.build(suggestion: SampleData.summaryOutput().speakerNames[0])
    #expect(
      options.filter { $0.id == Self.jerome.id.uuidString }.map(\.tag) == [.voice(.likely)])
  }

  @Test func aNilOrEmptyGuessAddsNothing() {
    let none = Self.build(suggestion: nil)
    #expect(Self.build(suggestion: Self.suggestion(nil)) == none)
    #expect(Self.build(suggestion: Self.suggestion("")) == none)
    #expect(Self.build(suggestion: Self.suggestion("  ")) == none)
    #expect(!none.contains { $0.tag == .mentioned })
  }

  @Test func aQuerySearchesAllPersonsNotOnlyRecent() {
    #expect(!Self.build().contains { $0.id == Self.anna.id.uuidString })
    let options = Self.build(query: "berg")
    #expect(
      options == [
        Option(kind: .person(Self.anna)),
        Option(kind: .create("berg")),
      ])
  }

  @Test func aSearchHitIsNotRepeatedAfterRecent() {
    let options = Self.build(recent: [Self.philipp], query: "phil")
    #expect(
      options.filter { $0.id == Self.philipp.id.uuidString } == [
        Option(kind: .person(Self.philipp))
      ])
  }

  // MARK: - Voice strength, the user's Noes and the last invitee

  @Test func voiceStrengthHasAFloorAndAStrongThreshold() {
    #expect(VoiceStrength.of(0.64) == nil)
    #expect(VoiceStrength.of(0.65) == .likely)
    #expect(VoiceStrength.of(0.74) == .likely)
    #expect(VoiceStrength.of(0.75) == .strong)
  }

  @Test func theVoiceMatchCarriesItsStrengthAndIsHiddenBelowTheFloor() {
    var speaker = Self.speakerTwo
    speaker.assignment = .suggested(personID: Self.jerome.id, similarity: 0.8)
    #expect(Self.build(speaker: speaker).first?.tag == .voice(.strong))

    speaker.assignment = .suggested(personID: Self.jerome.id, similarity: 0.5)
    let options = Self.build(speaker: speaker)
    #expect(!options.contains { if case .voice = $0.tag { true } else { false } })
    #expect(
      options.first == Option(kind: .person(Self.jerome), tag: .lastInvitee),
      "Jérôme is still the invitee nobody is named as")
  }

  @Test func aRejectedPersonIsOfferedOnlyWhenTyped() {
    let options = Self.build(rejected: [Self.jerome.id])
    #expect(!options.contains { $0.id == Self.jerome.id.uuidString }, "neither voice nor invitee")
    #expect(options == [Option(kind: .person(Self.nicolai), tag: .inThisMeeting)])

    let typed = Self.build(rejected: [Self.jerome.id], query: "jér")
    #expect(
      typed.contains(Option(kind: .person(Self.jerome))),
      "typed, without a reason: the voice match was answered No")
  }

  @Test func aRejectedNameHidesANamesakeToo() {
    let namesake = Person(
      id: SampleData.uuid(14), displayName: "Anna Berg", createdAt: SampleData.createdAt)
    var speaker = Self.speakerTwo
    speaker.assignment = .unknown
    let options = Self.build(
      speaker: speaker, persons: Self.persons + [namesake],
      suggestion: Self.suggestion("anna berg"), rejected: [namesake.id])
    #expect(!options.contains { Person.namesMatch($0.displayName, "Anna Berg") })
  }

  @Test func theOnlyInviteeForTheOnlyOpenVoiceIsTheLastInvitee() {
    let mia = Participant(
      id: SampleData.uuid(33), meetingID: SampleData.meetingID, displayName: "Mia Kurz",
      role: .them)
    let tom = Participant(
      id: SampleData.uuid(34), meetingID: SampleData.meetingID, displayName: "Tom Ruh",
      role: .them)
    #expect(Self.build(participants: [mia]).contains { $0.tag == .lastInvitee })

    let two = Self.build(participants: [mia, tom])
    #expect(two.filter { $0.tag == .invited }.map(\.displayName) == ["Mia Kurz", "Tom Ruh"])

    let another = Speaker(
      id: SampleData.uuid(23), meetingID: SampleData.meetingID, clusterLabel: "Speaker 3",
      clusterConfidence: 0.5)
    #expect(
      Self.build(speakers: Self.speakers + [another], participants: [mia])
        .contains(Option(kind: .create("Mia Kurz"), tag: .invited)),
      "another voice is still open")

    var dismissed = another
    dismissed.dismissed = true
    #expect(
      Self.build(speakers: Self.speakers + [dismissed], participants: [mia])
        .contains(Option(kind: .create("Mia Kurz"), tag: .lastInvitee)),
      "a dismissed voice is not open")

    let me = LaneMerger.meSpeaker(meetingID: SampleData.meetingID, personID: nil)
    #expect(
      Self.build(speakers: Self.speakers + [me], participants: [mia])
        .contains(Option(kind: .create("Mia Kurz"), tag: .lastInvitee)),
      "no invitee is me")
  }

  /// The last invitee is counted once per name, however many participant
  /// rows name them, and outranks the summary having mentioned them. Rust:
  /// `the_last_invitee_counts_names_once_and_outranks_mentioned`.
  @Test func theLastInviteeCountsNamesOnceAndOutranksMentioned() {
    let mia = Participant(
      id: SampleData.uuid(33), meetingID: SampleData.meetingID, displayName: "Mia Kurz",
      role: .them)
    let again = Participant(
      id: SampleData.uuid(34), meetingID: SampleData.meetingID, displayName: " mia kurz ",
      role: .them)
    let options = Self.build(participants: [mia, again], suggestion: Self.suggestion("Mia Kurz"))
    let mias = options.filter { Person.namesMatch($0.displayName, "Mia Kurz") }
    #expect(mias == [Option(kind: .create("Mia Kurz"), tag: .lastInvitee)])
  }
}
