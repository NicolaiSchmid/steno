import Foundation
import StenoBridge
import StenoCore
import Testing

/// Records what the bridge asked and answers with what a test set. No web
/// view anywhere in this file: the WebKit glue is exercised by the app and
/// `WebShellTests`; these are the pure rules under it.
@MainActor
private final class RecordingHost: BridgeHost {
  var reply: Result<JSONValue?, any Error> = .success(nil)
  private(set) var requests: [BridgeRequest] = []
  private(set) var sinks: [any BridgeEventSink] = []

  func handle(_ request: BridgeRequest) async throws -> JSONValue? {
    requests.append(request)
    return try reply.get()
  }

  func attach(_ events: any BridgeEventSink) {
    sinks.append(events)
  }
}

@MainActor
private final class RecordingSink: BridgeEventSink {
  private(set) var events: [BridgeEvent] = []

  func emit(_ event: BridgeEvent) {
    events.append(event)
  }
}

private struct Boom: Error, CustomStringConvertible {
  var description: String { "the disk is full" }
}

// MARK: - BundledSite

@Suite struct BundledSiteTests {
  let site = BundledSite(
    root: URL(fileURLWithPath: "/Applications/Steno.app/Contents/Resources/Web"))

  @Test func rootAndExtensionlessRoutesFallBackToIndex() {
    for path in ["", "/", "/shell", "/settings/general", "/meetings/"] {
      let resource = site.resolve(path: path)
      #expect(resource?.fileURL.lastPathComponent == "index.html", Comment(rawValue: path))
      #expect(resource?.contentType == "text/html; charset=utf-8", Comment(rawValue: path))
    }
  }

  @Test func filesResolveUnderTheRootWithTheirType() {
    let resource = site.resolve(path: "/assets/index-Bf3k.js")
    #expect(resource?.fileURL.path == "\(site.root.path)/assets/index-Bf3k.js")
    #expect(resource?.contentType == "text/javascript; charset=utf-8")
    #expect(site.resolve(path: "/assets/app.css")?.contentType == "text/css; charset=utf-8")
    #expect(site.resolve(path: "/icon.svg")?.contentType == "image/svg+xml")
    #expect(site.resolve(path: "/fonts/inter.WOFF2")?.contentType == "font/woff2")
    #expect(
      site.resolve(path: "/assets/index.js.map")?.contentType == "application/json; charset=utf-8")
  }

  @Test func theTypeTableIsClosed() {
    #expect(site.resolve(path: "/assets/module.wasm") == nil)
    #expect(site.resolve(path: "/photo.jpg") == nil)
  }

  @Test func traversalAndHiddenFilesAreRefused() {
    for path in [
      "/../Info.plist", "/assets/../../Info.plist", "/./index.html", "/.env", "/assets/.hidden.js",
      "/a\0b.js",
    ] {
      #expect(site.resolve(path: path) == nil, Comment(rawValue: path))
    }
  }

  @Test func headersCarryThePolicyAndNoCaching() throws {
    let resource = try #require(site.resolve(path: "/index.html"))
    let headers = site.headers(for: resource, length: 1234)
    #expect(headers["Content-Type"] == "text/html; charset=utf-8")
    #expect(headers["Content-Length"] == "1234")
    #expect(headers["Cache-Control"] == "no-store")
    #expect(
      headers["Content-Security-Policy"]
        == "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; "
        + "img-src 'self' data: blob:; font-src 'self'; connect-src 'none'")
    let notFound = BundledSite.notFoundHeaders(length: 9)
    #expect(notFound["Content-Security-Policy"] == BundledSite.contentSecurityPolicy)
    #expect(notFound["Content-Type"] == "text/plain; charset=utf-8")
  }
}

// MARK: - WebNavigationPolicy

@Suite struct WebNavigationPolicyTests {
  @Test func theBundleOnlyAllowsItsOwnOrigin() throws {
    let policy = WebNavigationPolicy.bundled
    #expect(policy.allows(try #require(URL(string: "steno-app://app/index.html#/shell"))))
    #expect(policy.allows(try #require(URL(string: "STENO-APP://APP/assets/a.js"))))
    #expect(policy.allows(try #require(URL(string: "about:blank"))))
    for blocked in [
      "https://example.com/", "http://localhost:5173/", "file:///etc/hosts",
      "steno-app://other/index.html", "javascript:alert(1)", "about:srcdoc",
    ] {
      #expect(!policy.allows(try #require(URL(string: blocked))), Comment(rawValue: blocked))
    }
    #expect(
      policy.startURL(route: "#/shell?tab=transcript").absoluteString
        == "steno-app://app/index.html#/shell?tab=transcript")
    #expect(policy.startURL(route: "").absoluteString == "steno-app://app/index.html")
  }

  @Test func theDevServerIsAnAllowedOriginWithItsExactPort() throws {
    let policy = WebNavigationPolicy.fromEnvironment(["STENO_WEB_DEV_URL": "http://localhost:5173"])
    #expect(policy.devServer != nil)
    #expect(policy.allows(try #require(URL(string: "http://localhost:5173/@vite/client"))))
    #expect(policy.allows(try #require(URL(string: "steno-app://app/index.html"))))
    #expect(!policy.allows(try #require(URL(string: "http://localhost:5174/"))))
    #expect(!policy.allows(try #require(URL(string: "https://localhost:5173/"))))
    #expect(policy.startURL(route: "#/shell").absoluteString == "http://localhost:5173/#/shell")
  }

  @Test func anUnusableVariableFallsBackToTheBundle() {
    #expect(WebNavigationPolicy.fromEnvironment([:]) == .bundled)
    #expect(WebNavigationPolicy.fromEnvironment(["STENO_WEB_DEV_URL": "  "]) == .bundled)
    #expect(WebNavigationPolicy.fromEnvironment(["STENO_WEB_DEV_URL": "5173"]) == .bundled)
  }
}

// MARK: - BridgeDispatcher

@Suite @MainActor struct BridgeDispatcherTests {
  @Test func theHandlerNameMatchesTheWebTransport() {
    #expect(BridgeDispatcher.messageHandlerName == "steno")
  }

  @Test func aRequestReachesTheHostAndItsResultIsEchoedWithTheID() async {
    let host = RecordingHost()
    host.reply = .success(.object(["prefill": .string("Anna")]))
    let body: [String: Any] = [
      "id": "req-7", "method": "speakers.options", "params": ["query": "An"],
    ]
    let reply = await BridgeDispatcher.dispatch(body, host: host)
    #expect(
      host.requests == [
        BridgeRequest(
          id: "req-7", method: .speakersOptions, params: .object(["query": .string("An")]))
      ])
    #expect(reply == BridgeReply(id: "req-7", result: .object(["prefill": .string("Anna")])))
  }

  @Test func nullParamsDecodeAsNoParams() async {
    let host = RecordingHost()
    let body: [String: Any] = ["id": "req-1", "method": "page.ready", "params": NSNull()]
    let reply = await BridgeDispatcher.dispatch(body, host: host)
    #expect(host.requests == [BridgeRequest(id: "req-1", method: .pageReady, params: nil)])
    #expect(reply == BridgeReply(id: "req-1"))
  }

  @Test func anUnknownMethodIsATypedErrorNotADecodingFailure() async {
    let host = RecordingHost()
    let body: [String: Any] = ["id": "req-2", "method": "meetings.explode", "params": NSNull()]
    let reply = await BridgeDispatcher.dispatch(body, host: host)
    #expect(host.requests.isEmpty)
    #expect(reply.id == "req-2")
    #expect(reply.result == nil)
    #expect(reply.error?.code == .unknownMethod)
    #expect(reply.error?.message.contains("meetings.explode") == true)
  }

  @Test func aMalformedBodyGetsInvalidParamsWithWhateverIDWasReadable() async {
    let host = RecordingHost()
    let string = await BridgeDispatcher.dispatch("hello", host: host)
    #expect(string.id == "")
    #expect(string.error?.code == .invalidParams)

    let nothing = await BridgeDispatcher.dispatch(nil, host: host)
    #expect(nothing.id == "")
    #expect(nothing.error?.code == .invalidParams)

    let noMethod = await BridgeDispatcher.dispatch(["id": "req-3"] as [String: Any], host: host)
    #expect(noMethod.id == "req-3")
    #expect(noMethod.error?.code == .invalidParams)

    let numericID = await BridgeDispatcher.dispatch(
      ["id": 4, "method": "page.ready"] as [String: Any], host: host)
    #expect(numericID.id == "")
    #expect(numericID.error?.code == .invalidParams)
    #expect(host.requests.isEmpty)
  }

  @Test func hostErrorsBecomeEnvelopes() async {
    let host = RecordingHost()
    let body: [String: Any] = [
      "id": "req-5", "method": "meetings.select", "params": ["meetingID": "x"],
    ]

    host.reply = .failure(BridgeError(code: .notFound, message: "No meeting with that id."))
    let typed = await BridgeDispatcher.dispatch(body, host: host)
    #expect(
      typed
        == BridgeReply(
          id: "req-5", error: BridgeError(code: .notFound, message: "No meeting with that id.")))

    host.reply = .failure(Boom())
    let untyped = await BridgeDispatcher.dispatch(body, host: host)
    #expect(untyped.error?.code == .failed)
    #expect(untyped.error?.message == "the disk is full")
  }

  @Test func theReplyObjectIsJSONNotAString() throws {
    let error = BridgeReply(id: "req-2", error: BridgeError(code: .notFound, message: "Gone."))
    let object = try #require(BridgeDispatcher.replyObject(for: error) as? [String: Any])
    #expect(object["id"] as? String == "req-2")
    #expect(object["result"] == nil)
    let nested = try #require(object["error"] as? [String: Any])
    #expect(nested["code"] as? String == "notFound")
    #expect(nested["message"] as? String == "Gone.")

    let ok = BridgeReply(id: "req-1", result: .array([.number(1), .bool(true), .null]))
    let okObject = try #require(BridgeDispatcher.replyObject(for: ok) as? [String: Any])
    #expect(okObject["error"] == nil)
    let result = try #require(okObject["result"] as? [Any])
    #expect(result.count == 3)
    #expect(result[2] is NSNull)

    let empty = try #require(
      BridgeDispatcher.replyObject(for: BridgeReply(id: "req-9")) as? [String: Any])
    #expect(empty.keys.sorted() == ["id"])
  }
}

// MARK: - Emit

@Suite struct EmitStatementTests {
  @Test func theStatementCallsWindowStenoEmitWithAParsedLiteral() throws {
    let event = BridgeEvent(
      topic: .recording,
      payload: .object(["state": .string("idle"), "deniedPermissions": .array([])]))
    #expect(
      try BridgeDispatcher.emitStatement(for: event)
        == #"window.steno.emit("recording", JSON.parse("{\"deniedPermissions\":[],\"state\":\"idle\"}"))"#
    )
  }

  @Test func quotesBackslashesNewlinesAndLineTerminatorsAreEscaped() throws {
    let literal = BridgeDispatcher.javaScriptStringLiteral(
      "say \"hi\"\\ok\nnext\r\ttab\u{2028}\u{2029}\u{1}é")
    #expect(literal == #""say \"hi\"\\ok\nnext\r\ttab\u2028\u2029\u0001é""#)

    let event = BridgeEvent(topic: .app, payload: .object(["title": .string("a\"b\\c\nd\u{2028}")]))
    let statement = try BridgeDispatcher.emitStatement(for: event)
    #expect(statement.hasPrefix(#"window.steno.emit("app", JSON.parse(""#))
    #expect(!statement.contains("\n"))
    #expect(!statement.contains("\u{2028}"))
    // The JSON inside the literal is itself escaped once more, so the page
    // parses back exactly the original string.
    #expect(statement.contains(#"a\\\"b\\\\c\\nd\u2028"#))
  }

  @Test func aSnapshotBecomesAnEventPayload() throws {
    let event = try BridgeEvent(topic: .recording, snapshot: BridgeSamples.recordingIdle)
    #expect(event.topic == .recording)
    #expect(event.payload["state"] == .string("idle"))
    #expect(event.payload["deniedPermissions"] == .array([]))
  }
}

// MARK: - PreviewBridgeHost

#if DEBUG
  @Suite @MainActor struct PreviewBridgeHostTests {
    @Test func pageReadyPublishesTheFourShellTopics() async throws {
      let host = PreviewBridgeHost()
      let sink = RecordingSink()
      host.attach(sink)
      let reply = await BridgeDispatcher.dispatch(
        ["id": "r1", "method": "page.ready", "params": NSNull()] as [String: Any], host: host)
      #expect(reply == BridgeReply(id: "r1"))
      #expect(sink.events.map(\.topic) == [.app, .recording, .meetingsList, .meetingDetail])
      #expect(sink.events[2].payload["groups"] != nil)
    }

    @Test func everythingElseIsUnknown() async {
      let host = PreviewBridgeHost()
      let layout = await BridgeDispatcher.dispatch(
        [
          "id": "r2", "method": "page.layout",
          "params": ["window": "main", "width": 1200, "height": 760],
        ]
          as [String: Any], host: host)
      #expect(layout == BridgeReply(id: "r2"))
      let other = await BridgeDispatcher.dispatch(
        ["id": "r3", "method": "recording.start", "params": NSNull()] as [String: Any], host: host)
      #expect(other.error?.code == .unknownMethod)
    }
  }
#endif
