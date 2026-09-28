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

  private static var digest: URL {
    resources.appendingPathComponent("AppIcon.sha256")
  }

  private static var mobileIcon: URL {
    TestSupport.repositoryRoot.appendingPathComponent("mobile/assets/icon.png")
  }

  private func bitmap(at url: URL) throws -> NSBitmapImageRep {
    let data = try Data(contentsOf: url)
    return try XCTUnwrap(NSBitmapImageRep(data: data), "\(url.lastPathComponent) is not a bitmap")
  }

  func testAppIconSetListsEveryFileAtItsPixelSize() throws {
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

    // A child the manifest does not name (a renamed slot's old PNG) makes
    // actool warn about an unassigned child on every build.
    let children = try FileManager.default.contentsOfDirectory(atPath: Self.iconSet.path)
    XCTAssertEqual(
      Set(children).subtracting(["Contents.json"]), filenames,
      "every child of the set is named in Contents.json")
  }

  /// The script renders the 32, 256 and 512 px slots twice under two names
  /// from one deterministic render; a hand-exported PNG almost never matches
  /// its twin byte for byte.
  func testTwinSlotsAreByteIdentical() throws {
    let twins = [
      ("icon_16x16@2x.png", "icon_32x32.png"),
      ("icon_128x128@2x.png", "icon_256x256.png"),
      ("icon_256x256@2x.png", "icon_512x512.png"),
    ]
    for (retina, plain) in twins {
      let first = try Data(contentsOf: Self.iconSet.appendingPathComponent(retina))
      let second = try Data(contentsOf: Self.iconSet.appendingPathComponent(plain))
      XCTAssertEqual(first, second, "\(retina) and \(plain) are the same render")
    }
  }

  func testBuiltAppCarriesTheIcon() throws {
    let info = try InfoPlistTests.appInfo()
    let named = [info["CFBundleIconName"], info["CFBundleIconFile"]].compactMap { $0 as? String }
    XCTAssertTrue(named.contains { $0.hasPrefix("AppIcon") }, "Info.plist names AppIcon: \(named)")

    let icns = TestSupport.builtApp.appendingPathComponent("Contents/Resources/AppIcon.icns")
    XCTAssertTrue(
      FileManager.default.fileExists(atPath: icns.path), "AppIcon.icns is in the bundle")
    let data = try Data(contentsOf: icns)
    XCTAssertEqual(String(decoding: data.prefix(4), as: UTF8.self), "icns", "icns magic bytes")
    // The container is a list of (4-byte type, 4-byte big-endian length)
    // elements after the 8-byte header. A Debug build's actool thins the
    // icns to the elements the build machine's displays need (ic04, ic11,
    // ic07 and ic13 on CI: 16, 32, 128 and 256 px); only a Release archive
    // carries all ten, so the pixel sizes are checked against the committed
    // set above and this test asks only for a real icns with image elements,
    // not the empty one an appiconset without files compiles to.
    var types: [String] = []
    var offset = 8
    while offset + 8 <= data.count {
      let type = String(decoding: data[offset..<offset + 4], as: UTF8.self)
      let length = data[offset + 4..<offset + 8].reduce(0) { $0 << 8 | Int($1) }
      guard length >= 8 else { break }
      types.append(type)
      offset += length
    }
    let imageTypes: Set<String> = [
      "ic04", "ic05", "icp4", "icp5", "icp6", "ic07", "ic08", "ic09", "ic10", "ic11", "ic12",
      "ic13", "ic14", "is32", "il32", "ih32", "it32",
    ]
    XCTAssertFalse(
      types.filter(imageTypes.contains).isEmpty, "the icns carries image elements: \(types)")
    let image = try XCTUnwrap(NSImage(contentsOf: icns), "AppIcon.icns decodes")
    XCTAssertFalse(image.representations.isEmpty, "AppIcon.icns has at least one representation")
  }

  /// The SVG is the source of truth and `make-app-icon.sh` writes its digest
  /// beside it. An edited SVG without regenerated PNGs fails here, since
  /// `--check` never runs on CI.
  func testIconSourceDigestIsCommitted() throws {
    let data = try Data(contentsOf: Self.source)
    let digest = SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    let recorded = try String(contentsOf: Self.digest, encoding: .utf8)
      .trimmingCharacters(in: .whitespacesAndNewlines)
    XCTAssertEqual(
      digest, recorded, "AppIcon.svg changed; run apps/macos/scripts/make-app-icon.sh")
  }

  /// `make-app-icon.sh` derives the iOS icon by rewriting these literal
  /// attribute values with `sed`. The script refuses a rewrite that matched
  /// nothing, but only on the regenerating machine. The two `id`s are not
  /// read by the script; they address the groups a menu bar template export
  /// would use (plan, open questions).
  func testIconSourceKeepsTheAttributesTheIOSRenderRewrites() throws {
    let text = try String(contentsOf: Self.source, encoding: .utf8)
    for attribute in ["viewBox=\"0 0 1024 1024\"", "rx=\"185\"", "rx=\"183\""] {
      XCTAssertTrue(
        text.contains(attribute),
        "\(attribute) is what the iOS sed in make-app-icon.sh rewrites; update both")
    }
    for id in ["id=\"glyph\"", "id=\"live\""] {
      XCTAssertTrue(text.contains(id), "\(id) names the group a template export addresses")
    }
  }

  /// App Store Connect rejects a 1024 icon with an alpha channel; the
  /// script's `%[opaque]` check only runs on the regenerating machine.
  func testMobileIconIsOpaque1024() throws {
    let rep = try bitmap(at: Self.mobileIcon)
    XCTAssertEqual(rep.pixelsWide, 1024)
    XCTAssertEqual(rep.pixelsHigh, 1024)
    XCTAssertFalse(rep.hasAlpha, "mobile/assets/icon.png carries no alpha channel")
  }

  /// The iOS render sets the viewBox to the plate, so the plate's top
  /// gradient stop (`#303034`) reaches the top corners. Had the `sed`
  /// matched nothing, the fill background (`#141416`) would sit there
  /// instead. The bottom stop equals that background, so only the top
  /// corners can tell.
  func testMobileIconPlateFillsTheSquare() throws {
    let rep = try bitmap(at: Self.mobileIcon)
    for x in [0, 1023] {
      let color = try XCTUnwrap(
        rep.colorAt(x: x, y: 0)?.usingColorSpace(.sRGB), "pixel (\(x), 0) decodes")
      XCTAssertGreaterThan(
        color.redComponent, 0.1, "pixel (\(x), 0) carries the plate, not the fill: \(color)")
    }
  }
}
