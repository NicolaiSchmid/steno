import Foundation

extension ProcessingPipeline {
  /// `DeliveryDispatcher.deliverAll`: never throws, every destination's
  /// outcome is a `Delivery` row.
  func deliver(meetingID: UUID) async {
    let dispatcher = dependencies.delivery
    _ = try? await run(.deliver, meetingID: meetingID) {
      await dispatcher.deliverAll(meetingID: meetingID)
    }
  }
}
