import Foundation
import Testing

@testable import StenoCore

@Suite struct EventBusTests {
  static func progress(_ stage: PipelineStage, fraction: Double = 0) -> MeetingEvent {
    .progress(
      meetingID: SampleData.meetingID,
      progress: ProcessingProgress(
        stage: stage, fraction: fraction, nextFraction: min(1, fraction + 0.1),
        estimatedRemaining: .seconds(90), isEstimateSeeded: true))
  }

  @Test func onePostReachesTwoSubscribers() async throws {
    let bus = MeetingEventBus()
    let first = await bus.subscribe()
    let second = await bus.subscribe()

    let event = Self.progress(.decode)
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
    await bus.post(Self.progress(.decode))
    let stream = await bus.subscribe()
    await bus.post(Self.progress(.transcribe, fraction: 0.1))
    var iterator = stream.makeAsyncIterator()
    #expect(await iterator.next() == Self.progress(.transcribe, fraction: 0.1))
  }

  /// `finish()` ends every live subscription: each stream still delivers
  /// what was posted before, then returns nil; a post after it reaches
  /// nobody; a subscription made after it is a fresh one and sees later
  /// posts.
  @Test func finishEndsEverySubscriptionAndLaterPostsReachNobody() async throws {
    let bus = MeetingEventBus()
    let first = await bus.subscribe()
    let second = await bus.subscribe()
    await bus.post(Self.progress(.decode))
    await bus.post(Self.progress(.transcribe, fraction: 0.1))
    await bus.finish()
    await bus.post(Self.progress(.diarize, fraction: 0.5))

    for stream in [first, second] {
      var iterator = stream.makeAsyncIterator()
      #expect(await iterator.next() == Self.progress(.decode))
      #expect(await iterator.next() == Self.progress(.transcribe, fraction: 0.1))
      #expect(await iterator.next() == nil, "the stream ends after what was posted before finish")
    }

    let late = await bus.subscribe()
    await bus.post(Self.progress(.cleanup, fraction: 0.7))
    var iterator = late.makeAsyncIterator()
    #expect(await iterator.next() == Self.progress(.cleanup, fraction: 0.7))
    await bus.finish()
    #expect(await iterator.next() == nil, "a second finish ends the fresh subscription too")
  }
}
