import Foundation

@testable import StenoCore

extension MeetingEventBus {
  /// Everything posted to `stream` so far: posts a sentinel and reads up to
  /// it, so a run that posted fewer events than expected fails an assertion
  /// instead of hanging the test.
  func drain(_ stream: AsyncStream<MeetingEvent>) async -> [MeetingEvent] {
    let sentinel = MeetingEvent.speakersNeedReview(meetingID: UUID(), speakerIDs: [])
    post(sentinel)
    var iterator = stream.makeAsyncIterator()
    var collected: [MeetingEvent] = []
    while let event = await iterator.next(), event != sentinel { collected.append(event) }
    return collected
  }
}
