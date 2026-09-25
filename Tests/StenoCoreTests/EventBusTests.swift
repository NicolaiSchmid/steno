import Foundation
import Testing

@testable import StenoCore

@Suite struct EventBusTests {
  @Test func onePostReachesTwoSubscribers() async throws {
    let bus = MeetingEventBus()
    let first = await bus.subscribe()
    let second = await bus.subscribe()

    let event = MeetingEvent.progress(meetingID: SampleData.meetingID, stage: .decode, fraction: 0)
    await bus.post(event)
    await bus.post(
      .speakersNeedReview(meetingID: SampleData.meetingID, speakerIDs: [SampleData.speakerTwoID]))

    var firstIterator = first.makeAsyncIterator()
    var secondIterator = second.makeAsyncIterator()
    #expect(await firstIterator.next() == event)
    #expect(await secondIterator.next() == event)
    #expect(
      await firstIterator.next()
        == .speakersNeedReview(
          meetingID: SampleData.meetingID, speakerIDs: [SampleData.speakerTwoID]))
    #expect(
      await secondIterator.next()
        == .speakersNeedReview(
          meetingID: SampleData.meetingID, speakerIDs: [SampleData.speakerTwoID]))
  }

  @Test func lateSubscribersMissEarlierEvents() async throws {
    let bus = MeetingEventBus()
    await bus.post(.progress(meetingID: SampleData.meetingID, stage: .decode, fraction: 0))
    let stream = await bus.subscribe()
    await bus.post(.progress(meetingID: SampleData.meetingID, stage: .transcribe, fraction: 0.1))
    var iterator = stream.makeAsyncIterator()
    #expect(
      await iterator.next()
        == .progress(meetingID: SampleData.meetingID, stage: .transcribe, fraction: 0.1))
  }
}
