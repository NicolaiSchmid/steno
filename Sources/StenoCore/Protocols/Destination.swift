/// A one-way push target. Constructed with its typed settings, renders its
/// own artefacts from the `MeetingExport`. `previous == nil` is the initial
/// delivery; otherwise the destination overwrites the files it wrote before
/// and touches nothing else.
public protocol Destination: Sendable {
  var id: String { get }
  func validate() async throws
  func deliver(_ meeting: MeetingExport, previous: DeliveryReceipt?) async throws -> DeliveryReceipt
}
