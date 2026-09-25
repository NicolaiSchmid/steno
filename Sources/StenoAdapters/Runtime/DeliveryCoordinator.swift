import Foundation
import StenoCore

/// The `DeliveryDispatcher`: exports the meeting once, then runs every
/// configured destination in order with one `Delivery` row per (meeting,
/// destination), handing each its stored receipt as `previous`. Never
/// throws; a failed export or destination is a `.failed` row and the next
/// destination still runs. There is no separate re-export path:
/// `ProcessingPipeline.redeliver` calls `deliverAll` again.
public actor DeliveryCoordinator: DeliveryDispatcher {
  let store: MeetingStore
  let settings: SettingsStore
  let destinations: @Sendable (Settings) -> [any Destination]
  let now: @Sendable () -> Date

  public init(
    store: MeetingStore,
    settings: SettingsStore,
    destinations: @escaping @Sendable (Settings) -> [any Destination] = StenoAdapters
      .destinations(for:),
    now: @escaping @Sendable () -> Date = Date.init
  ) {
    self.store = store
    self.settings = settings
    self.destinations = destinations
    self.now = now
  }

  public func deliverAll(meetingID: UUID) async -> [Delivery] {
    guard let settings = try? await settings.load() else { return [] }
    let targets = destinations(settings)
    guard !targets.isEmpty else { return [] }
    let existing = (try? await store.deliveries(meetingID: meetingID)) ?? []
    let export: Result<MeetingExport, any Error>
    do {
      export = .success(try await store.export(meetingID: meetingID))
    } catch {
      export = .failure(error)
    }

    var results: [Delivery] = []
    for destination in targets {
      let previous = existing.first { $0.destinationID == destination.id }?.receipt
      var delivery = Delivery(
        meetingID: meetingID, destinationID: destination.id, status: .pending,
        lastAttemptAt: now(), receipt: previous)
      switch export {
      case .failure(let error):
        delivery.status = .failed("export failed: \(String(describing: error))")
      case .success(let export):
        try? await store.save(delivery)
        do {
          delivery.receipt = try await destination.deliver(export, previous: previous)
          delivery.status = .delivered
        } catch {
          delivery.status = .failed(String(describing: error))
        }
      }
      try? await store.save(delivery)
      results.append(delivery)
    }
    return results
  }
}
