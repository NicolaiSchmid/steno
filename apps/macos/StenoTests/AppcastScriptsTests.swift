import XCTest

/// The rolling appcast: `merge-appcast.py` folds a release into the feed
/// newest first with the channel applied, `publish-appcast.sh` creates and
/// then updates the `appcast` branch on a real (bare) origin, and the
/// release workflow runs it after the release is public.
final class AppcastScriptsTests: XCTestCase {
  private static var scripts: URL {
    TestSupport.appRoot.appendingPathComponent("scripts", isDirectory: true)
  }

  private static func appcast(version: Int, short: String, tag: String) -> String {
    appcast(items: [(version: version, short: short, tag: tag)])
  }

  /// A per-release appcast as `generate_appcast` writes it, with one
  /// `<item>` per entry.
  private static func appcast(items: [(version: Int, short: String, tag: String)]) -> String {
    let body = items.map { item in
      """
              <item>
                  <title>\(item.short)</title>
                  <link>https://github.com/NicolaiSchmid/steno/releases</link>
                  <sparkle:version>\(item.version)</sparkle:version>
                  <sparkle:shortVersionString>\(item.short)</sparkle:shortVersionString>
                  <sparkle:minimumSystemVersion>15.0</sparkle:minimumSystemVersion>
                  <enclosure url="https://github.com/NicolaiSchmid/steno/releases/download/\(item.tag)/Steno-\(item.short).dmg" length="1" type="application/octet-stream" sparkle:edSignature="c2ln"/>
              </item>
      """
    }.joined(separator: "\n")
    return """
      <?xml version="1.0" standalone="yes"?>
      <rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" version="2.0">
          <channel>
              <title>Steno</title>
      \(body)
          </channel>
      </rss>

      """
  }

  @discardableResult
  private func run(
    _ executable: String, _ arguments: [String], environment: [String: String] = [:],
    directory: URL? = nil
  ) throws -> (status: Int32, output: String) {
    let process = Process()
    process.executableURL = URL(fileURLWithPath: executable)
    process.arguments = arguments
    process.environment = ["PATH": "/usr/bin:/bin:/usr/local/bin"].merging(environment) { $1 }
    if let directory { process.currentDirectoryURL = directory }
    let pipe = Pipe()
    process.standardOutput = pipe
    process.standardError = pipe
    try process.run()
    let data = pipe.fileHandleForReading.readDataToEndOfFile()
    process.waitUntilExit()
    return (process.terminationStatus, String(decoding: data, as: UTF8.self))
  }

  private func versions(in appcast: String) -> [String] {
    let pattern = try! NSRegularExpression(pattern: "<sparkle:version>(\\d+)</sparkle:version>")
    return pattern.matches(in: appcast, range: NSRange(appcast.startIndex..., in: appcast))
      .compactMap { Range($0.range(at: 1), in: appcast).map { String(appcast[$0]) } }
  }

  /// The text of each `<item>`, in document order.
  private func items(in appcast: String) -> [String] {
    appcast.components(separatedBy: "<item>").dropFirst()
      .map { $0.components(separatedBy: "</item>")[0] }
  }

  // MARK: merge-appcast.py

  func testMergeFoldsNewestFirstAndTagsTheChannel() throws {
    let dir = try TestSupport.temporaryDirectory("steno-appcast")
    defer { try? FileManager.default.removeItem(at: dir) }
    let rc1 = dir.appendingPathComponent("rc1.xml")
    let stable = dir.appendingPathComponent("stable.xml")
    let rc2 = dir.appendingPathComponent("rc2.xml")
    let rolling = dir.appendingPathComponent("appcast.xml")
    try Self.appcast(version: 244, short: "0.9.0-rc.1", tag: "v0.9.0-rc.1").write(
      to: rc1, atomically: true, encoding: .utf8)
    try Self.appcast(version: 260, short: "0.9.0", tag: "v0.9.0").write(
      to: stable, atomically: true, encoding: .utf8)
    try Self.appcast(version: 270, short: "0.9.1-rc.1", tag: "v0.9.1-rc.1").write(
      to: rc2, atomically: true, encoding: .utf8)
    let merge = Self.scripts.appendingPathComponent("merge-appcast.py").path

    // First release: no rolling file yet.
    var result = try run(
      "/usr/bin/env", ["python3", merge, rolling.path, rc1.path, rolling.path, "--channel", "beta"])
    XCTAssertEqual(result.status, 0, result.output)
    var feed = try String(contentsOf: rolling, encoding: .utf8)
    XCTAssertEqual(versions(in: feed), ["244"])
    XCTAssertTrue(feed.contains("<sparkle:channel>beta</sparkle:channel>"), feed)

    // A stable release lands on the default channel, above the candidate.
    result = try run("/usr/bin/env", ["python3", merge, rolling.path, stable.path, rolling.path])
    XCTAssertEqual(result.status, 0, result.output)
    feed = try String(contentsOf: rolling, encoding: .utf8)
    XCTAssertEqual(versions(in: feed), ["260", "244"])
    XCTAssertEqual(feed.components(separatedBy: "<sparkle:channel>").count - 1, 1)

    // A newer candidate, twice: replaced, not duplicated.
    for _ in 0..<2 {
      result = try run(
        "/usr/bin/env",
        ["python3", merge, rolling.path, rc2.path, rolling.path, "--channel", "beta"])
      XCTAssertEqual(result.status, 0, result.output)
    }
    feed = try String(contentsOf: rolling, encoding: .utf8)
    XCTAssertEqual(versions(in: feed), ["270", "260", "244"])
    XCTAssertEqual(feed.components(separatedBy: "<sparkle:channel>").count - 1, 2)
    XCTAssertTrue(feed.contains("sparkle:edSignature=\"c2ln\""), "signatures are carried verbatim")
    XCTAssertTrue(
      feed.contains("xmlns:sparkle=\"http://www.andymatuschak.org/xml-namespaces/sparkle\""))

    // The cap keeps the newest.
    result = try run(
      "/usr/bin/env", ["python3", merge, rolling.path, rc1.path, rolling.path, "--max-items", "2"])
    XCTAssertEqual(result.status, 0, result.output)
    feed = try String(contentsOf: rolling, encoding: .utf8)
    XCTAssertEqual(versions(in: feed), ["270", "260"])
  }

  /// A release that carries two items (a re-run of `generate_appcast` over a
  /// dist folder with two DMGs) adds both; items only the rolling feed knows
  /// stay; the order is by build number, not by arrival; the channel lands
  /// on the release's items alone; the namespace declaration survives; and
  /// the same merge twice is byte-identical, which is what lets
  /// `publish-appcast.sh` skip an empty commit.
  func testMergeKeepsRollingItemsAndAddsEveryReleaseItem() throws {
    let dir = try TestSupport.temporaryDirectory("steno-appcast")
    defer { try? FileManager.default.removeItem(at: dir) }
    let rolling = dir.appendingPathComponent("appcast.xml")
    let release = dir.appendingPathComponent("release.xml")
    try Self.appcast(items: [
      (version: 244, short: "0.9.0-rc.1", tag: "v0.9.0-rc.1"),
      (version: 260, short: "0.9.0", tag: "v0.9.0"),
    ]).write(to: rolling, atomically: true, encoding: .utf8)
    try Self.appcast(items: [
      (version: 250, short: "0.9.0-rc.2", tag: "v0.9.0-rc.2"),
      (version: 270, short: "0.9.1-rc.1", tag: "v0.9.1-rc.1"),
    ]).write(to: release, atomically: true, encoding: .utf8)
    let merge = Self.scripts.appendingPathComponent("merge-appcast.py").path
    let arguments = [
      "python3", merge, rolling.path, release.path, rolling.path, "--channel", "beta",
    ]

    let result = try run("/usr/bin/env", arguments)
    XCTAssertEqual(result.status, 0, result.output)
    XCTAssertTrue(result.output.contains("4 item(s), newest build 270"), result.output)
    let feed = try String(contentsOf: rolling, encoding: .utf8)
    XCTAssertEqual(versions(in: feed), ["270", "260", "250", "244"])
    let entries = items(in: feed)
    XCTAssertEqual(entries.count, 4)
    for item in entries {
      let version = versions(in: item).first ?? "?"
      let tagged = item.contains("<sparkle:channel>beta</sparkle:channel>")
      XCTAssertEqual(tagged, ["270", "250"].contains(version), "build \(version)")
      XCTAssertTrue(
        item.contains("sparkle:edSignature=\"c2ln\""), "build \(version) lost its signature")
    }
    XCTAssertEqual(
      feed.components(
        separatedBy: "xmlns:sparkle=\"http://www.andymatuschak.org/xml-namespaces/sparkle\""
      )
      .count - 1, 1, "the namespace is declared once, on <rss>")
    XCTAssertTrue(feed.hasPrefix("<?xml"), String(feed.prefix(40)))
    XCTAssertTrue(feed.hasSuffix("\n"), "ends with a newline so git shows a clean diff")

    let again = try run("/usr/bin/env", arguments)
    XCTAssertEqual(again.status, 0, again.output)
    let rerun = try String(contentsOf: rolling, encoding: .utf8)
    XCTAssertEqual(rerun, feed, "the same release again changes nothing")
  }

  func testMergeRefusesADifferentReleaseWithTheSameBuildNumber() throws {
    let dir = try TestSupport.temporaryDirectory("steno-appcast")
    defer { try? FileManager.default.removeItem(at: dir) }
    let rc = dir.appendingPathComponent("rc.xml")
    let stable = dir.appendingPathComponent("stable.xml")
    let rolling = dir.appendingPathComponent("appcast.xml")
    try Self.appcast(version: 244, short: "0.9.0-rc.1", tag: "v0.9.0-rc.1").write(
      to: rc, atomically: true, encoding: .utf8)
    try Self.appcast(version: 244, short: "0.9.0", tag: "v0.9.0").write(
      to: stable, atomically: true, encoding: .utf8)
    let merge = Self.scripts.appendingPathComponent("merge-appcast.py").path
    var result = try run("/usr/bin/env", ["python3", merge, rolling.path, rc.path, rolling.path])
    XCTAssertEqual(result.status, 0, result.output)
    let before = try String(contentsOf: rolling, encoding: .utf8)

    // Promoting the candidate on the same commit: Sparkle orders by build
    // number alone, so the stable item would hide from rc installs.
    result = try run("/usr/bin/env", ["python3", merge, rolling.path, stable.path, rolling.path])
    XCTAssertNotEqual(result.status, 0)
    XCTAssertTrue(result.output.contains("::error::build 244 is already published"), result.output)
    XCTAssertEqual(
      try String(contentsOf: rolling, encoding: .utf8), before, "the feed is untouched")

    // The same release again (a re-run of the tag) still replaces cleanly.
    result = try run("/usr/bin/env", ["python3", merge, rolling.path, rc.path, rolling.path])
    XCTAssertEqual(result.status, 0, result.output)
  }

  func testMergeRejectsAnItemWithoutABuildNumber() throws {
    let dir = try TestSupport.temporaryDirectory("steno-appcast")
    defer { try? FileManager.default.removeItem(at: dir) }
    let broken = dir.appendingPathComponent("broken.xml")
    try Self.appcast(version: 1, short: "x", tag: "vx")
      .replacingOccurrences(of: "<sparkle:version>1</sparkle:version>", with: "")
      .write(to: broken, atomically: true, encoding: .utf8)
    let merge = Self.scripts.appendingPathComponent("merge-appcast.py").path
    let out = dir.appendingPathComponent("out.xml")
    let result = try run("/usr/bin/env", ["python3", merge, out.path, broken.path, out.path])
    XCTAssertNotEqual(result.status, 0)
    XCTAssertTrue(result.output.contains("::error::"), result.output)
    XCTAssertFalse(FileManager.default.fileExists(atPath: out.path))
  }

  // MARK: publish-appcast.sh

  private struct Origin {
    var origin: URL
    var clone: URL
    /// A HOME of its own and a fixed author, so no user config leaks in.
    var git: [String: String]
  }

  /// A bare origin with `main` pushed from `clone`, as the release job sees
  /// its checkout.
  private func seedOrigin(in dir: URL) throws -> Origin {
    let origin = dir.appendingPathComponent("origin.git", isDirectory: true)
    let clone = dir.appendingPathComponent("clone", isDirectory: true)
    let home = dir.appendingPathComponent("home", isDirectory: true)
    try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
    let git = [
      "HOME": home.path,
      "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.com",
      "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.com",
      "GIT_CONFIG_NOSYSTEM": "1",
    ]
    func run(_ arguments: [String], in directory: URL? = nil) throws {
      let result = try self.run("/usr/bin/git", arguments, environment: git, directory: directory)
      XCTAssertEqual(result.status, 0, "git \(arguments.joined(separator: " ")): \(result.output)")
    }
    try run(["init", "--quiet", "--bare", origin.path])
    try run(["clone", "--quiet", origin.path, clone.path])
    try "hello".write(
      to: clone.appendingPathComponent("README.md"), atomically: true, encoding: .utf8)
    try run(["add", "README.md"], in: clone)
    try run(["commit", "--quiet", "-m", "init"], in: clone)
    try run(["push", "--quiet", "origin", "HEAD:refs/heads/main"], in: clone)
    return Origin(origin: origin, clone: clone, git: git)
  }

  private func publishEnvironment(_ seeded: Origin, release: URL, scratch: URL) -> [String: String]
  {
    seeded.git.merging([
      "STENO_REPO_ROOT": seeded.clone.path, "STENO_RELEASE_APPCAST": release.path,
      "RUNNER_TEMP": scratch.path,
    ]) { $1 }
  }

  func testPublishCreatesThenUpdatesTheBranchOnOrigin() throws {
    let dir = try TestSupport.temporaryDirectory("steno-publish")
    defer { try? FileManager.default.removeItem(at: dir) }
    let seeded = try seedOrigin(in: dir)
    let release1 = dir.appendingPathComponent("release1.xml")
    let release2 = dir.appendingPathComponent("release2.xml")
    try Self.appcast(version: 244, short: "0.9.0-rc.1", tag: "v0.9.0-rc.1").write(
      to: release1, atomically: true, encoding: .utf8)
    try Self.appcast(version: 260, short: "0.9.0", tag: "v0.9.0").write(
      to: release2, atomically: true, encoding: .utf8)
    let publish = Self.scripts.appendingPathComponent("publish-appcast.sh").path

    var result = try run(
      "/bin/bash", [publish, "v0.9.0-rc.1", "true"],
      environment: publishEnvironment(seeded, release: release1, scratch: dir))
    XCTAssertEqual(result.status, 0, result.output)
    XCTAssertTrue(result.output.contains("does not exist yet; creating it"), result.output)

    result = try run(
      "/bin/bash", [publish, "v0.9.0", "false"],
      environment: publishEnvironment(seeded, release: release2, scratch: dir))
    XCTAssertEqual(result.status, 0, result.output)
    XCTAssertTrue(result.output.contains("exists; updating"), result.output)

    let feed = try run(
      "/usr/bin/git", ["show", "appcast:appcast.xml"], environment: seeded.git,
      directory: seeded.origin)
    XCTAssertEqual(feed.status, 0, feed.output)
    XCTAssertEqual(versions(in: feed.output), ["260", "244"])
    XCTAssertEqual(feed.output.components(separatedBy: "<sparkle:channel>beta").count - 1, 1)
    let tree = try run(
      "/usr/bin/git", ["ls-tree", "--name-only", "appcast"], environment: seeded.git,
      directory: seeded.origin)
    XCTAssertEqual(
      Set(tree.output.split(separator: "\n").map(String.init)), ["README.md", "appcast.xml"],
      "the orphan branch carries only the feed and a note")
    let worktrees = try run(
      "/usr/bin/git", ["worktree", "list"], environment: seeded.git, directory: seeded.clone)
    XCTAssertEqual(
      worktrees.output.split(separator: "\n").count, 1, "the temporary worktree was removed")
  }

  /// A re-run of the release job for the same tag (a retried workflow, or
  /// the rehearsal's second pass) finds the feed already holding the item:
  /// it says so, exits 0, and pushes no commit, so the branch tip and the
  /// history stay as they were.
  func testPublishRerunOnTheSameTagPushesNothing() throws {
    let dir = try TestSupport.temporaryDirectory("steno-publish")
    defer { try? FileManager.default.removeItem(at: dir) }
    let seeded = try seedOrigin(in: dir)
    let release = dir.appendingPathComponent("release.xml")
    try Self.appcast(version: 244, short: "0.9.0-rc.1", tag: "v0.9.0-rc.1").write(
      to: release, atomically: true, encoding: .utf8)
    let publish = Self.scripts.appendingPathComponent("publish-appcast.sh").path
    let environment = publishEnvironment(seeded, release: release, scratch: dir)
    func tip() throws -> String {
      let result = try run(
        "/usr/bin/git", ["rev-parse", "refs/heads/appcast"], environment: seeded.git,
        directory: seeded.origin)
      XCTAssertEqual(result.status, 0, result.output)
      return result.output.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    var result = try run("/bin/bash", [publish, "v0.9.0-rc.1", "true"], environment: environment)
    XCTAssertEqual(result.status, 0, result.output)
    XCTAssertTrue(result.output.contains("published v0.9.0-rc.1 to appcast"), result.output)
    let first = try tip()
    XCTAssertEqual(first.count, 40, first)

    result = try run("/bin/bash", [publish, "v0.9.0-rc.1", "true"], environment: environment)
    XCTAssertEqual(result.status, 0, result.output)
    XCTAssertTrue(result.output.contains("exists; updating"), result.output)
    XCTAssertTrue(result.output.contains("appcast unchanged for v0.9.0-rc.1"), result.output)
    XCTAssertFalse(result.output.contains("published"), result.output)
    let second = try tip()
    XCTAssertEqual(second, first, "nothing was pushed")
    let count = try run(
      "/usr/bin/git", ["rev-list", "--count", "refs/heads/appcast"], environment: seeded.git,
      directory: seeded.origin)
    XCTAssertEqual(count.output.trimmingCharacters(in: .whitespacesAndNewlines), "1")
    let worktrees = try run(
      "/usr/bin/git", ["worktree", "list"], environment: seeded.git, directory: seeded.clone)
    XCTAssertEqual(
      worktrees.output.split(separator: "\n").count, 1, "the early exit still removes the worktree")
  }

  // MARK: release.yml

  func testReleaseWorkflowPublishesTheFeedAfterTheRelease() throws {
    let workflow = try String(
      contentsOf: TestSupport.repositoryRoot.appendingPathComponent(
        ".github/workflows/release.yml"),
      encoding: .utf8)
    let publishRelease = try XCTUnwrap(
      workflow.range(of: "- name: Publish GitHub release")?.lowerBound)
    let rolling = try XCTUnwrap(workflow.range(of: "publish-appcast.sh")?.lowerBound)
    let homebrew = try XCTUnwrap(workflow.range(of: "bump-homebrew-cask.sh")?.lowerBound)
    XCTAssertLessThan(publishRelease, rolling, "the feed announces a release that is public")
    XCTAssertLessThan(rolling, homebrew)
    XCTAssertTrue(workflow.contains("permissions:\n  contents: write"), "the push needs contents")
    XCTAssertTrue(
      workflow.contains("git tag --points-at HEAD 'v*'"),
      "two tags on one commit share a build number; the version step refuses the second")

    let spec = try String(
      contentsOf: TestSupport.appRoot.appendingPathComponent("project.yml"), encoding: .utf8)
    XCTAssertTrue(
      spec.contains(
        "SUFeedURL: https://raw.githubusercontent.com/NicolaiSchmid/steno/appcast/appcast.xml"))
    XCTAssertFalse(spec.contains("releases/latest/download/appcast.xml"))
  }
}
