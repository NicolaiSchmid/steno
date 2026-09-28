import AppKit
import CryptoKit
import XCTest

/// The app icon, as committed and as built. Hostless like `InfoPlistTests`:
/// the committed appiconset and the SVG source are read from the source tree
/// beside this file, the built `Steno.app` beside the test bundle. The trap
/// this guards: a valid `Contents.json` with no image files compiles silently
/// and macOS shows the generic icon, which is exactly how the repository
/// shipped before `.plans/2026-09-28-app-icon.md`.
final class AppIconTests: XCTestCase {
  private static var resources: URL {
    TestSupport.appRoot.appendingPathComponent("Steno/Resources", isDirectory: true)
  }

  private static var iconSet: URL {
    resources.appendingPathComponent("Assets.xcassets/AppIcon.appiconset", isDirectory: true)
  }

  private static var source: URL {
    resources.appendingPathComponent("AppIcon.svg")
  }

  private static var mobileIcon: URL {
    TestSupport.repositoryRoot.appendingPathComponent("mobile/assets/icon.png")
  }

  private func bitmap(at url: URL) throws -> NSBitmapImageRep {
    let data = try Data(contentsOf: url)
    return try XCTUnwrap(NSBitmapImageRep(data: data), "\(url.lastPathComponent) is not a bitmap")
  }

  func testAppIconSetListsEveryFileAndEveryFileExists() throws {
    let data = try Data(contentsOf: Self.iconSet.appendingPathComponent("Contents.json"))
    let json = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
    let images = try XCTUnwrap(json["images"] as? [[String: Any]])
    XCTAssertEqual(images.count, 10, "five mac sizes at 1x and 2x")
    var filenames: Set<String> = []
    for image in images {
      let filename = try XCTUnwrap(image["filename"] as? String, "\(image) names its file")
      XCTAssertFalse(filename.isEmpty)
      filenames.insert(filename)
      XCTAssertEqual(image["idiom"] as? String, "mac")
      let size = try XCTUnwrap(image["size"] as? String)
      let scale = try XCTUnwrap(image["scale"] as? String)
      let points = try XCTUnwrap(Int(size.split(separator: "x")[0]), size)
      let factor = try XCTUnwrap(Int(scale.dropLast()), scale)
      let url = Self.iconSet.appendingPathComponent(filename)
      XCTAssertTrue(FileManager.default.fileExists(atPath: url.path), "\(filename) exists")
      let rep = try bitmap(at: url)
      XCTAssertEqual(rep.pixelsWide, points * factor, "\(filename) width")
      XCTAssertEqual(rep.pixelsHigh, points * factor, "\(filename) height")
    }
    XCTAssertEqual(filenames.count, 10, "every entry has its own file")
  }

  func testBuiltAppCarriesTheIcon() throws {
    let info = try InfoPlistTests.appInfo()
    let named = [info["CFBundleIconName"], info["CFBundleIconFile"]].compactMap { $0 as? String }
    XCTAssertTrue(named.contains { $0.hasPrefix("AppIcon") }, "Info.plist names AppIcon: \(named)")

    let icns = InfoPlistTests.builtApp.appendingPathComponent("Contents/Resources/AppIcon.icns")
    XCTAssertTrue(
      FileManager.default.fileExists(atPath: icns.path), "AppIcon.icns is in the bundle")
    let header = try Data(contentsOf: icns).prefix(4)
    XCTAssertEqual(String(decoding: header, as: UTF8.self), "icns", "icns magic bytes")
    let image = try XCTUnwrap(NSImage(contentsOf: icns), "AppIcon.icns decodes")
    let widths = image.representations.map(\.pixelsWide)
    XCTAssertTrue(widths.contains(1024), "the 512@2x representation is present: \(widths)")
  }

  /// The SVG is the source of truth and `make-app-icon.sh` writes its digest
  /// beside the PNGs. An edited SVG without regenerated PNGs fails here,
  /// since `--check` never runs on CI.
  func testIconSourceIsCommittedBesideTheSet() throws {
    let data = try Data(contentsOf: Self.source)
    let text = String(decoding: data, as: UTF8.self)
    XCTAssertTrue(text.contains("id=\"glyph\""), "the glyph group keeps its id")
    XCTAssertTrue(text.contains("id=\"live\""), "the live line keeps its id")
    let digest = SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    let recorded = try String(
      contentsOf: Self.iconSet.appendingPathComponent("SOURCE.sha256"), encoding: .utf8
    ).trimmingCharacters(in: .whitespacesAndNewlines)
    XCTAssertEqual(
      digest, recorded, "AppIcon.svg changed; run apps/macos/scripts/make-app-icon.sh")
  }

  /// App Store Connect rejects a 1024 icon with an alpha channel; the
  /// script's `%[opaque]` check only runs on the regenerating machine.
  func testMobileIconIsOpaque1024() throws {
    let rep = try bitmap(at: Self.mobileIcon)
    XCTAssertEqual(rep.pixelsWide, 1024)
    XCTAssertEqual(rep.pixelsHigh, 1024)
    XCTAssertFalse(rep.hasAlpha, "mobile/assets/icon.png carries no alpha channel")
  }
}
