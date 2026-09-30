import StenoAdapters
import StenoCore
import XCTest

/// The pure labels the detail pane's footer still takes from the app: the
/// destination's display name. The states table, the header's Stop rule and
/// the meta line moved to the web UI with WP2 of the webview plan; the
/// snapshot mapping the page renders is covered by `MainWindowSnapshotsTests`.
final class DetailStatesTests: XCTestCase {
  /// The footer names the destination, never its storage id.
  func testDeliveryNamesItsDestination() {
    let obsidian = Delivery(
      meetingID: SampleData.meetingID, destinationID: ObsidianFolderDestination.destinationID,
      status: .delivered)
    XCTAssertEqual(obsidian.destinationDisplayName, "Obsidian")
    let unknown = Delivery(
      meetingID: SampleData.meetingID, destinationID: "notion", status: .pending)
    XCTAssertEqual(
      unknown.destinationDisplayName, "notion", "an unknown destination has only its id")
  }
}
