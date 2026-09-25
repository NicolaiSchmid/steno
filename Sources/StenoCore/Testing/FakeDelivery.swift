import Foundation

/// A `Destination` that writes `meeting.json` under `<root>/<meetingID>/` and
/// records every export it received.
public struct RecordingDestination: Destination, Sendable {
  public static let rendererVersion = 1

  public let id: String
  public var root: URL
  public var validateFailure: (any Error & Sendable)?
  public var deliverFailure: (any Error & Sendable)?
  public let deliveries = CallLog<MeetingExport>()

  public init(
    id: String = "recording", root: URL,
    validateFailure: (any Error & Sendable)? = nil,
    deliverFailure: (any Error & Sendable)? = nil
  ) {
    self.id = id
    self.root = root
    self.validateFailure = validateFailure
    self.deliverFailure = deliverFailure
  }

  public func validate() async throws {
    if let validateFailure { throw validateFailure }
  }

  public func deliver(_ meeting: MeetingExport, previous: DeliveryReceipt?) async throws
    -> DeliveryReceipt
  {
    await deliveries.record(meeting)
    if let deliverFailure { throw deliverFailure }
    let folder = previous?.folder ?? meeting.meeting.id.uuidString
    let directory = root.appendingPathComponent(folder, isDirectory: true)
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    let data = try StenoJSON.encode(meeting)
    try data.write(to: directory.appendingPathComponent("meeting.json"), options: .atomic)
    return DeliveryReceipt(
      root: root.path,
      folder: folder,
      files: [
        DeliveredFile(
          relativePath: "meeting.json", ownership: .owned, sha256: ContentHash.sha256(data))
      ],
      rendererVersion: Self.rendererVersion
    )
  }

  /// The `meeting.json` this destination wrote for `meetingID`.
  public func exportURL(meetingID: UUID) -> URL {
    root.appendingPathComponent(meetingID.uuidString, isDirectory: true)
      .appendingPathComponent("meeting.json")
  }
}

/// A `DeliveryDispatcher` over a fixed list of destinations: exports the
/// meeting once, passes each destination its stored receipt as `previous`,
/// saves one `Delivery` per destination and never throws.
public struct RecordingDispatcher: DeliveryDispatcher, Sendable {
  public var store: MeetingStore
  public var destinations: [any Destination]
  public var now: @Sendable () -> Date
  public let calls = CallLog<UUID>()

  public init(
    store: MeetingStore, destinations: [any Destination],
    now: @escaping @Sendable () -> Date = Date.init
  ) {
    self.store = store
    self.destinations = destinations
    self.now = now
  }

  public func deliverAll(meetingID: UUID) async -> [Delivery] {
    await calls.record(meetingID)
    guard let export = try? await store.export(meetingID: meetingID) else { return [] }
    let existing = (try? await store.deliveries(meetingID: meetingID)) ?? []
    var results: [Delivery] = []
    for destination in destinations {
      let previous = existing.first { $0.destinationID == destination.id }
      var delivery = Delivery(
        id: previous?.id ?? MeetingStore.derivedID(meetingID, salt: "delivery-\(destination.id)"),
        meetingID: meetingID,
        destinationID: destination.id,
        status: .pending,
        lastAttemptAt: now(),
        receipt: previous?.receipt
      )
      do {
        delivery.receipt = try await destination.deliver(export, previous: previous?.receipt)
        delivery.status = .delivered
      } catch {
        delivery.status = .failed(String(describing: error))
      }
      try? await store.save(delivery)
      results.append(delivery)
    }
    return results
  }
}

/// A `HandoverIntake` that records every admission and returns a fixed or
/// fresh meeting id.
public actor FakeHandoverIntake: HandoverIntake {
  public struct Admission: Sendable, Equatable {
    public var file: URL
    public var metadata: RecordingMetadata
    public var device: PairedDevice
  }

  public private(set) var admissions: [Admission] = []
  public var meetingID: UUID?
  public var failure: (any Error & Sendable)?

  public init(meetingID: UUID? = nil, failure: (any Error & Sendable)? = nil) {
    self.meetingID = meetingID
    self.failure = failure
  }

  public func admit(file: URL, metadata: RecordingMetadata, device: PairedDevice) async throws
    -> UUID
  {
    admissions.append(Admission(file: file, metadata: metadata, device: device))
    if let failure { throw failure }
    return meetingID ?? UUID()
  }
}
