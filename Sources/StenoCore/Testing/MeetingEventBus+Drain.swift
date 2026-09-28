import Foundation

extension MeetingEventBus {
  /// Everything posted to `stream` so far: posts a sentinel and reads up to
  /// it, so a run that posted fewer events than expected fails an assertion
  /// instead of hanging the test. Test support for StenoCore and the app.
  public func drain(_ stream: AsyncStream<MeetingEvent>) async -> [MeetingEvent] {
    let sentinel = MeetingEvent.speakersNeedReview(meetingID: UUID(), speakerIDs: [])
    post(sentinel)
    var iterator = stream.makeAsyncIterator()
    var collected: [MeetingEvent] = []
    while let event = await iterator.next(), event != sentinel { collected.append(event) }
    return collected
  }
}

extension MeetingEvent {
  /// The value of a `progress` event, nil for every other event.
  var progress: ProcessingProgress? {
    if case .progress(_, let progress) = self { return progress }
    return nil
  }

  /// The stage of a `progress` event, nil for every other event; tests
  /// compare stages where the fraction and the remaining time are not the
  /// point.
  var stage: PipelineStage? { progress?.stage }
}
