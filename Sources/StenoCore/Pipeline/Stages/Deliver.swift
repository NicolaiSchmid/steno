import Foundation

extension ProcessingPipeline {
  /// `DeliveryDispatcher.deliverAll`: never throws, every destination's
  /// outcome is a `Delivery` row.
  func deliver(meetingID: UUID) async {
    await post(.deliver, meetingID: meetingID)
    _ = await dependencies.dispatcher.deliverAll(meetingID: meetingID)
  }
}
