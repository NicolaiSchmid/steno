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
    destinations: @escaping @Sendable (Settings) -> [any Destination] = DeliveryCoordinator
      .destinations(for:),
    now: @escaping @Sendable () -> Date = Date.init
  ) {
    self.store = store
    self.settings = settings
    self.destinations = destinations
    self.now = now
  }

  /// Every destination the settings configure, in delivery order. Today the
  /// Obsidian folder when `settings.obsidian` is set; a second destination
  /// adds one line here and one typed optional to `Settings`.
  public static func destinations(for settings: Settings) -> [any Destination] {
    guard let obsidian = settings.obsidian else { return [] }
    return [ObsidianFolderDestination(settings: obsidian)]
  }

  public func deliverAll(meetingID: UUID) async -> [Delivery] {
    let existing = (try? await store.deliveries(meetingID: meetingID)) ?? []
    let settings: Settings
    do {
      settings = try await self.settings.load()
    } catch {
      // Settings that do not load cannot say where to deliver. Every
      // destination delivered before is told so; silence would leave the
      // meeting `.ready` with stale rows and no trace of the failure.
      var results: [Delivery] = []
      for var delivery in existing {
        delivery.status = .failed("settings failed: \(String(describing: error))")
        delivery.lastAttemptAt = now()
        try? await store.save(delivery)
        results.append(delivery)
      }
      return results
    }
    let targets = destinations(settings)
    guard !targets.isEmpty else { return [] }
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
