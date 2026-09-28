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
    query: String = ""
  ) -> [Option] {
    SpeakerOptions.build(
      speaker: speaker, speakers: speakers, persons: persons, participants: participants,
      suggestion: suggestion, recent: recent, query: query)
  }

  static func suggestion(_ name: String?) -> SpeakerNameSuggestion {
    SpeakerNameSuggestion(
      speakerID: speakerTwo.id, name: name, confidence: 0.8, evidence: "Speaker 1 says the name.")
  }

  @Test func orderIsVoiceMatchGuessAttendeesThisMeetingThenRecent() {
    let attendee = Participant(
      id: SampleData.uuid(32), meetingID: SampleData.meetingID, personID: Self.anna.id,
      displayName: "Anna Berg", role: .them)
    let options = Self.build(
      participants: SampleData.participants() + [attendee],
      suggestion: Self.suggestion("Ben"),
      recent: [Self.jerome, Self.nicolai, Self.philipp])
    #expect(
      options == [
        Option(kind: .person(Self.jerome), tag: .soundsLike),
        Option(kind: .create("Ben"), tag: .mentioned),
        Option(kind: .person(Self.anna), tag: .attendee),
        Option(kind: .person(Self.nicolai), tag: .inThisMeeting),
        Option(kind: .person(Self.philipp)),
      ])
  }

  @Test func theSuggestedPersonIsNotRepeatedUnderRecent() {
    let options = Self.build(recent: [Self.jerome, Self.philipp])
    #expect(options.filter { $0.id == Self.jerome.id.uuidString }.count == 1)
    #expect(options.first == Option(kind: .person(Self.jerome), tag: .soundsLike))
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
    #expect(options.contains(Option(kind: .create("Mia Kurz"), tag: .attendee)))
    #expect(options.first { $0.id == "create:Mia Kurz" } != nil)
  }

  @Test func anAttendeeWithoutAPersonIDResolvesByName() {
    // A calendar attendee "philipp schroder" is offered as the existing
    // person Philipp Schröder, never as a create row beside them.
    let attendee = Participant(
      id: SampleData.uuid(35), meetingID: SampleData.meetingID, displayName: "philipp schroder",
      role: .them)
    let options = Self.build(participants: [attendee])
    #expect(options.contains(Option(kind: .person(Self.philipp), tag: .attendee)))
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
    let options = Self.build(speaker: speaker, suggestion: Self.suggestion("jerome"))
    #expect(
      options == [
        Option(kind: .person(Self.jerome), tag: .mentioned),
        Option(kind: .person(Self.nicolai), tag: .inThisMeeting),
      ])
  }

  @Test func theLLMGuessIsACreateRowWhenNobodyHasTheName() {
    let options = Self.build(suggestion: Self.suggestion(" Ben "))
    #expect(options[1] == Option(kind: .create("Ben"), tag: .mentioned))
    #expect(options[1].id == "create:Ben")
  }

  @Test func theVoiceMatchWinsOverTheSameLLMGuess() {
    let options = Self.build(suggestion: SampleData.summaryOutput().speakerNames[0])
    #expect(options.filter { $0.id == Self.jerome.id.uuidString }.map(\.tag) == [.soundsLike])
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

  @Test func tagsCarryTheirLabels() {
    #expect(Option.Tag.soundsLike.rawValue == "Sounds like")
    #expect(Option.Tag.mentioned.rawValue == "Mentioned")
    #expect(Option.Tag.attendee.rawValue == "Attendee")
    #expect(Option.Tag.inThisMeeting.rawValue == "In this meeting")
  }
}
