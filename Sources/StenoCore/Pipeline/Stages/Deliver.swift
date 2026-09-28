import Foundation

extension ProcessingPipeline {
  /// `DeliveryDispatcher.deliverAll`: never throws, every destination's
  /// outcome is a `Delivery` row.
  func deliver(meetingID: UUID) async {
    let dispatcher = dependencies.dispatcher
    await run(.deliver, meetingID: meetingID) {
      _ = await dispatcher.deliverAll(meetingID: meetingID)
    }
  }
}
