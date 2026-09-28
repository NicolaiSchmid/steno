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
    """
    <?xml version="1.0" standalone="yes"?>
    <rss xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" version="2.0">
        <channel>
            <title>Steno</title>
            <item>
                <title>\(short)</title>
                <link>https://github.com/NicolaiSchmid/steno/releases</link>
                <sparkle:version>\(version)</sparkle:version>
                <sparkle:shortVersionString>\(short)</sparkle:shortVersionString>
                <sparkle:minimumSystemVersion>15.0</sparkle:minimumSystemVersion>
                <enclosure url="https://github.com/NicolaiSchmid/steno/releases/download/\(tag)/Steno-\(short).dmg" length="1" type="application/octet-stream" sparkle:edSignature="c2ln"/>
            </item>
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

  func testPublishCreatesThenUpdatesTheBranchOnOrigin() throws {
    let dir = try TestSupport.temporaryDirectory("steno-publish")
    defer { try? FileManager.default.removeItem(at: dir) }
    let origin = dir.appendingPathComponent("origin.git", isDirectory: true)
    let clone = dir.appendingPathComponent("clone", isDirectory: true)
    let home = dir.appendingPathComponent("home", isDirectory: true)
    try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
    let gitEnvironment = [
      "HOME": home.path,
      "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.com",
      "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.com",
      "GIT_CONFIG_NOSYSTEM": "1",
    ]
    func git(_ arguments: [String], in directory: URL? = nil) throws {
      let result = try run(
        "/usr/bin/git", arguments, environment: gitEnvironment, directory: directory)
      XCTAssertEqual(result.status, 0, "git \(arguments.joined(separator: " ")): \(result.output)")
    }
    try git(["init", "--quiet", "--bare", origin.path])
    try git(["clone", "--quiet", origin.path, clone.path])
    try "hello".write(
      to: clone.appendingPathComponent("README.md"), atomically: true, encoding: .utf8)
    try git(["add", "README.md"], in: clone)
    try git(["commit", "--quiet", "-m", "init"], in: clone)
    try git(["push", "--quiet", "origin", "HEAD:refs/heads/main"], in: clone)

    let release1 = dir.appendingPathComponent("release1.xml")
    let release2 = dir.appendingPathComponent("release2.xml")
    try Self.appcast(version: 244, short: "0.9.0-rc.1", tag: "v0.9.0-rc.1").write(
      to: release1, atomically: true, encoding: .utf8)
    try Self.appcast(version: 260, short: "0.9.0", tag: "v0.9.0").write(
      to: release2, atomically: true, encoding: .utf8)
    let publish = Self.scripts.appendingPathComponent("publish-appcast.sh").path

    var result = try run(
      "/bin/bash", [publish, "v0.9.0-rc.1", "true"],
      environment: gitEnvironment.merging([
        "STENO_REPO_ROOT": clone.path, "STENO_RELEASE_APPCAST": release1.path,
        "RUNNER_TEMP": dir.path,
      ]) { $1 })
    XCTAssertEqual(result.status, 0, result.output)
    XCTAssertTrue(result.output.contains("does not exist yet; creating it"), result.output)

    result = try run(
      "/bin/bash", [publish, "v0.9.0", "false"],
      environment: gitEnvironment.merging([
        "STENO_REPO_ROOT": clone.path, "STENO_RELEASE_APPCAST": release2.path,
        "RUNNER_TEMP": dir.path,
      ]) { $1 })
    XCTAssertEqual(result.status, 0, result.output)
    XCTAssertTrue(result.output.contains("exists; updating"), result.output)

    let feed = try run(
      "/usr/bin/git", ["show", "appcast:appcast.xml"], environment: gitEnvironment,
      directory: origin)
    XCTAssertEqual(feed.status, 0, feed.output)
    XCTAssertEqual(versions(in: feed.output), ["260", "244"])
    XCTAssertEqual(feed.output.components(separatedBy: "<sparkle:channel>beta").count - 1, 1)
    let tree = try run(
      "/usr/bin/git", ["ls-tree", "--name-only", "appcast"], environment: gitEnvironment,
      directory: origin)
    XCTAssertEqual(
      Set(tree.output.split(separator: "\n").map(String.init)), ["README.md", "appcast.xml"],
      "the orphan branch carries only the feed and a note")
    let worktrees = try run(
      "/usr/bin/git", ["worktree", "list"], environment: gitEnvironment, directory: clone)
    XCTAssertEqual(
      worktrees.output.split(separator: "\n").count, 1, "the temporary worktree was removed")
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

    let spec = try String(
      contentsOf: TestSupport.appRoot.appendingPathComponent("project.yml"), encoding: .utf8)
    XCTAssertTrue(
      spec.contains(
        "SUFeedURL: https://raw.githubusercontent.com/NicolaiSchmid/steno/appcast/appcast.xml"))
    XCTAssertFalse(spec.contains("releases/latest/download/appcast.xml"))
  }
}
