import Foundation
import StenoCore
import Testing

@testable import StenoLLM

extension JWTClaims {
  /// A JWT with `payload` and a throwaway header and signature; the store
  /// never checks the signature.
  static func unsignedToken(payload: JSONValue) -> String {
    let header = base64URLEncode(Data(#"{"alg":"none","typ":"JWT"}"#.utf8))
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
    let body = base64URLEncode((try? encoder.encode(payload)) ?? Data("{}".utf8))
    return "\(header).\(body).signature"
  }

  static func base64URLEncode(_ data: Data) -> String {
    data.base64EncodedString()
      .replacingOccurrences(of: "+", with: "-")
      .replacingOccurrences(of: "/", with: "_")
      .replacingOccurrences(of: "=", with: "")
  }
}

/// A temporary `CODEX_HOME` with an `auth.json` the tests write, and a stub
/// server standing in for the token endpoint.
final class CodexHome: Sendable {
  let directory: URL
  let server: StubChatServer
  /// A fixed "now" the store reads; `epoch` is what the tokens are minted
  /// relative to.
  static let now = Date(timeIntervalSince1970: 1_790_000_000)

  init() throws {
    directory = FileManager.default.temporaryDirectory.appendingPathComponent(
      "codex-home-\(UUID().uuidString)", isDirectory: true)
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    server = try StubChatServer()
  }

  var file: URL { directory.appendingPathComponent("auth.json") }

  func store(now: Date = CodexHome.now) -> CodexCredentialStore {
    CodexCredentialStore(
      home: directory, tokenEndpoint: server.baseURL.appendingPathComponent("oauth/token"),
      clientID: "app_test", now: { now })
  }

  func stop() {
    server.stop()
    try? FileManager.default.removeItem(at: directory)
  }

  /// An access token expiring `expiresIn` seconds after `now`.
  static func accessToken(expiresIn: TimeInterval, plan: String = "plus") -> String {
    JWTClaims.unsignedToken(
      payload: .object([
        "exp": .number(now.timeIntervalSince1970 + expiresIn),
        JWTClaims.openAIAuthClaim: .object([
          "chatgpt_account_id": .string("acct_jwt"),
          "chatgpt_plan_type": .string(plan),
        ]),
      ]))
  }

  static func idToken(email: String = "nicolai@example.com", plan: String = "plus") -> String {
    JWTClaims.unsignedToken(
      payload: .object([
        "email": .string(email),
        JWTClaims.openAIAuthClaim: .object([
          "chatgpt_account_id": .string("acct_jwt"),
          "chatgpt_plan_type": .string(plan),
        ]),
      ]))
  }

  /// Writes an `auth.json` like the CLI's, with an extra key Steno does not
  /// know, so the tests can check it survives a write-back.
  func write(
    access: String = CodexHome.accessToken(expiresIn: 3_600),
    refresh: String = "rt_original",
    id: String? = CodexHome.idToken(),
    accountID: String? = "acct_stored",
    lastRefresh: Date? = CodexHome.now.addingTimeInterval(-3_600),
    authMode: String? = "chatgpt",
    extra: [String: JSONValue] = ["agent_identity": .object(["keep": .bool(true)])]
  ) throws {
    var tokens: [String: JSONValue] = [
      "access_token": .string(access), "refresh_token": .string(refresh),
    ]
    if let id { tokens["id_token"] = .string(id) }
    if let accountID { tokens["account_id"] = .string(accountID) }
    var document: [String: JSONValue] = ["OPENAI_API_KEY": .null, "tokens": .object(tokens)]
    if let authMode { document["auth_mode"] = .string(authMode) }
    if let lastRefresh {
      document["last_refresh"] = .string(StenoJSON.format(lastRefresh))
    }
    for (key, value) in extra { document[key] = value }
    let data = try JSONEncoder().encode(JSONValue.object(document))
    try data.write(to: file)
    try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: file.path)
  }

  func document() throws -> [String: JSONValue] {
    guard
      case .object(let object) = try JSONDecoder().decode(
        JSONValue.self, from: Data(contentsOf: file))
    else { throw CodexCredentialError.malformed("not an object") }
    return object
  }
}

@Suite struct CodexCredentialStoreTests {
  @Test func readsTheFileTheWayTheCLIWritesIt() async throws {
    let home = try CodexHome()
    defer { home.stop() }
    try home.write()
    let credentials = try await home.store().stored()
    #expect(credentials.accessToken == CodexHome.accessToken(expiresIn: 3_600))
    #expect(credentials.refreshToken == "rt_original")
    #expect(credentials.accountID == "acct_stored", "the stored id wins over the claim")
    #expect(credentials.email == "nicolai@example.com")
    #expect(credentials.planType == "plus")
    #expect(credentials.expiresAt == CodexHome.now.addingTimeInterval(3_600))
    #expect(credentials.lastRefresh == CodexHome.now.addingTimeInterval(-3_600))
    #expect(credentials.accountLine == "nicolai@example.com (Plus)")
    // Fresh: no network, no write.
    let before = try Data(contentsOf: home.file)
    #expect(try await home.store().current() == credentials)
    #expect(try Data(contentsOf: home.file) == before)
    #expect(home.server.requests.isEmpty)
  }

  @Test func accountIDFallsBackToTheIDTokenClaim() async throws {
    let home = try CodexHome()
    defer { home.stop() }
    try home.write(accountID: nil)
    #expect(try await home.store().stored().accountID == "acct_jwt")
  }

  @Test func missingFileAPIKeyLoginAndMalformedFilesAreTold() async throws {
    let home = try CodexHome()
    defer { home.stop() }
    await #expect(throws: CodexCredentialError.notSignedIn) { try await home.store().stored() }

    try Data(#"{"OPENAI_API_KEY":"sk-x","tokens":null}"#.utf8).write(to: home.file)
    await #expect(throws: CodexCredentialError.apiKeyLogin) { try await home.store().stored() }

    try home.write(authMode: "apikey")
    await #expect(throws: CodexCredentialError.apiKeyLogin) { try await home.store().stored() }

    try Data("not json".utf8).write(to: home.file)
    let error = await #expect(throws: CodexCredentialError.self) {
      try await home.store().stored()
    }
    guard case .malformed? = error else {
      Issue.record("expected malformed, got \(String(describing: error))")
      return
    }
  }

  @Test func refreshesNearExpiryAndWritesBackPreservingUnknownKeys() async throws {
    let home = try CodexHome()
    defer { home.stop() }
    try home.write(access: CodexHome.accessToken(expiresIn: 120))
    let newAccess = CodexHome.accessToken(expiresIn: 3_600, plan: "pro")
    home.server.enqueue(
      Scripts.tokenRefresh(
        access: newAccess, refresh: "rt_rotated", id: CodexHome.idToken(plan: "pro")))

    let credentials = try await home.store().current()
    #expect(credentials.accessToken == newAccess)
    #expect(credentials.refreshToken == "rt_rotated")
    #expect(credentials.planType == "pro")
    #expect(credentials.lastRefresh == CodexHome.now)

    let request = try #require(home.server.requests.first)
    #expect(request.method == "POST")
    #expect(request.path == "/v1/oauth/token")
    let body = try JSONDecoder().decode([String: String].self, from: request.body)
    #expect(
      body == [
        "grant_type": "refresh_token", "client_id": "app_test", "refresh_token": "rt_original",
      ])

    let document = try home.document()
    guard case .object(let tokens)? = document["tokens"] else {
      Issue.record("tokens gone")
      return
    }
    #expect(tokens["refresh_token"] == .string("rt_rotated"))
    #expect(tokens["access_token"] == .string(newAccess))
    #expect(tokens["account_id"] == .string("acct_stored"), "untouched keys stay")
    #expect(document["agent_identity"] == .object(["keep": .bool(true)]), "unknown keys survive")
    #expect(document["auth_mode"] == .string("chatgpt"))
    #expect(document.keys.contains("OPENAI_API_KEY"))
    let attributes = try FileManager.default.attributesOfItem(atPath: home.file.path)
    #expect((attributes[.posixPermissions] as? Int) == 0o600)
    #expect(
      try FileManager.default.contentsOfDirectory(atPath: home.directory.path) == ["auth.json"],
      "no temp file left behind")
  }

  @Test func aStaleFileIsRefreshedEvenWithALiveToken() async throws {
    let home = try CodexHome()
    defer { home.stop() }
    try home.write(lastRefresh: CodexHome.now.addingTimeInterval(-9 * 24 * 3_600))
    home.server.enqueue(
      Scripts.tokenRefresh(access: CodexHome.accessToken(expiresIn: 3_600), refresh: "rt_2"))
    #expect(try await home.store().current().refreshToken == "rt_2")
    #expect(home.server.requests.count == 1)
  }

  @Test func aSpentRefreshTokenMeansSignInAgain() async throws {
    let home = try CodexHome()
    defer { home.stop() }
    try home.write(access: CodexHome.accessToken(expiresIn: 10))
    home.server.enqueue(Scripts.tokenRefreshRejected(code: "refresh_token_expired"))
    let error = await #expect(throws: CodexCredentialError.self) {
      try await home.store().current()
    }
    guard case .signInExpired(let detail)? = error else {
      Issue.record("expected signInExpired, got \(String(describing: error))")
      return
    }
    #expect(detail.contains("refresh_token_expired"))
    #expect(!detail.contains("rt_original"), "the refresh token never lands in an error")
    #expect(String(describing: error!).contains("codex login"))
    // The file is untouched by a failed refresh.
    #expect(try await home.store().stored().refreshToken == "rt_original")
  }

  @Test func aReusedTokenRereadsTheFileTheCLIMayHaveRotated() async throws {
    let home = try CodexHome()
    defer { home.stop() }
    try home.write(access: CodexHome.accessToken(expiresIn: 10))
    let fresh = CodexHome.accessToken(expiresIn: 3_600)
    home.server.respond { request in
      let body = (try? JSONDecoder().decode([String: String].self, from: request.body)) ?? [:]
      if body["refresh_token"] == "rt_original" {
        // Simulate the CLI having refreshed in between: it wrote a new token.
        try? home.write(access: CodexHome.accessToken(expiresIn: 10), refresh: "rt_from_cli")
        return Scripts.tokenRefreshRejected(code: "refresh_token_reused")
      }
      return Scripts.tokenRefresh(access: fresh, refresh: "rt_after_cli")
    }
    let credentials = try await home.store().current()
    #expect(credentials.refreshToken == "rt_after_cli")
    #expect(home.server.requests.count == 2)
  }

  @Test func aTransientRefreshFailureIsNotASignInProblem() async throws {
    let home = try CodexHome()
    defer { home.stop() }
    try home.write(access: CodexHome.accessToken(expiresIn: 10))
    home.server.enqueue(Scripts.serverError(503))
    let error = await #expect(throws: CodexCredentialError.self) {
      try await home.store().current()
    }
    guard case .refreshFailed? = error else {
      Issue.record("expected refreshFailed, got \(String(describing: error))")
      return
    }
  }

  @Test func defaultHomeHonoursCodexHome() async {
    #expect(
      CodexCredentialStore.defaultHome(environment: ["CODEX_HOME": "/tmp/elsewhere"]).path
        == "/tmp/elsewhere")
    #expect(CodexCredentialStore.defaultHome(environment: [:]).lastPathComponent == ".codex")
    // A trailing slash, as a shell export often has, does not double up;
    // an empty override is no override.
    let slashed = CodexCredentialStore.defaultHome(environment: ["CODEX_HOME": "/tmp/elsewhere/"])
    #expect(await CodexCredentialStore(home: slashed).fileURL.path == "/tmp/elsewhere/auth.json")
    #expect(
      CodexCredentialStore.defaultHome(environment: ["CODEX_HOME": ""]).lastPathComponent
        == ".codex")
  }

  /// Older CLI files have no `auth_mode`; tokens alone mean a ChatGPT login.
  @Test func aFileWithoutAuthModeIsAChatGPTLogin() async throws {
    let home = try CodexHome()
    defer { home.stop() }
    try home.write(authMode: nil)
    let credentials = try await home.store().stored()
    #expect(credentials.accountID == "acct_stored")
    #expect(credentials.email == "nicolai@example.com")
    // The mode is compared without regard to case.
    try home.write(authMode: "ChatGPT")
    #expect(try await home.store().stored().accountID == "acct_stored")
  }

  @Test func missingOrEmptyTokensAndNoAccountIDAreMalformed() async throws {
    let home = try CodexHome()
    defer { home.stop() }
    try home.write(id: nil, accountID: nil)
    await #expect(throws: CodexCredentialError.malformed("no account id")) {
      try await home.store().stored()
    }
    // An id token without the account claim is as good as none, and an
    // empty stored id does not shadow the claim.
    try home.write(
      id: JWTClaims.unsignedToken(payload: .object(["email": .string("a@b.c")])), accountID: nil)
    await #expect(throws: CodexCredentialError.malformed("no account id")) {
      try await home.store().stored()
    }
    try home.write(accountID: "")
    #expect(try await home.store().stored().accountID == "acct_jwt")
    try home.write(access: "")
    await #expect(throws: CodexCredentialError.malformed("no access token")) {
      try await home.store().stored()
    }
    try home.write(refresh: "")
    await #expect(throws: CodexCredentialError.malformed("no refresh token")) {
      try await home.store().stored()
    }
    // An access token that is not a JWT still works: no expiry, no plan.
    try home.write(access: "opaque-token")
    let opaque = try await home.store().stored()
    #expect(opaque.expiresAt == nil)
    #expect(opaque.planType == "plus", "from the id token")
    #expect(try await home.store().current() == opaque, "never refreshed by expiry")
    #expect(home.server.requests.isEmpty)
  }

  /// `needsRefresh` at its edges: no `last_refresh` (a file the CLI wrote
  /// at login and never refreshed) is not stale; exactly eight days is not
  /// stale; the token is refreshed strictly inside five minutes of `exp`.
  @Test func aFileNeverRefreshedIsNotStaleAndTheWindowsAreExact() async throws {
    let home = try CodexHome()
    defer { home.stop() }
    try home.write(lastRefresh: nil)
    let credentials = try await home.store().current()
    #expect(credentials.lastRefresh == nil)
    #expect(home.server.requests.isEmpty)

    try home.write(lastRefresh: CodexHome.now.addingTimeInterval(-8 * 24 * 3_600))
    _ = try await home.store().current()
    #expect(home.server.requests.isEmpty, "eight days is the limit, not past it")

    try home.write(access: CodexHome.accessToken(expiresIn: 300))
    _ = try await home.store().current()
    #expect(home.server.requests.isEmpty, "five minutes left is enough")

    try home.write(access: CodexHome.accessToken(expiresIn: 299))
    home.server.enqueue(
      Scripts.tokenRefresh(access: CodexHome.accessToken(expiresIn: 3_600), refresh: "rt_2"))
    #expect(try await home.store().current().refreshToken == "rt_2")
    #expect(home.server.requests.count == 1)

    // An already-expired token is refreshed too, not rejected.
    try home.write(access: CodexHome.accessToken(expiresIn: -3_600))
    home.server.enqueue(
      Scripts.tokenRefresh(access: CodexHome.accessToken(expiresIn: 3_600), refresh: "rt_3"))
    #expect(try await home.store().current().refreshToken == "rt_3")
  }

  /// The token endpoint may answer without a rotated refresh token or id
  /// token: the old ones stay in the file. And a `CODEX_HOME` holds more
  /// than `auth.json` (config, sessions, logs): the write-back leaves every
  /// other entry alone.
  @Test func aRefreshWithoutRotationKeepsTheOldTokensAndTouchesOnlyAuthJSON() async throws {
    let home = try CodexHome()
    defer { home.stop() }
    try home.write(access: CodexHome.accessToken(expiresIn: 10))
    let config = home.directory.appendingPathComponent("config.toml")
    let configBytes = Data("model = \"gpt-5.6-terra\"\n".utf8)
    try configBytes.write(to: config)
    let sessions = home.directory.appendingPathComponent("sessions", isDirectory: true)
    try FileManager.default.createDirectory(at: sessions, withIntermediateDirectories: true)
    try Data("{}".utf8).write(to: sessions.appendingPathComponent("rollout.jsonl"))
    let fresh = CodexHome.accessToken(expiresIn: 3_600)
    home.server.enqueue(Scripts.tokenRefresh(access: fresh))

    let credentials = try await home.store().current()
    #expect(credentials.accessToken == fresh)
    #expect(credentials.refreshToken == "rt_original")
    #expect(credentials.email == "nicolai@example.com")
    #expect(credentials.lastRefresh == CodexHome.now)
    let document = try home.document()
    guard case .object(let tokens)? = document["tokens"] else {
      Issue.record("tokens gone")
      return
    }
    #expect(tokens["access_token"] == .string(fresh))
    #expect(tokens["refresh_token"] == .string("rt_original"))
    #expect(tokens["id_token"] == .string(CodexHome.idToken()))
    #expect(tokens["account_id"] == .string("acct_stored"))
    #expect(
      try FileManager.default.contentsOfDirectory(atPath: home.directory.path).sorted() == [
        "auth.json", "config.toml", "sessions",
      ])
    #expect(try Data(contentsOf: config) == configBytes)
    #expect(try FileManager.default.contentsOfDirectory(atPath: sessions.path) == ["rollout.jsonl"])
    // The next read sees the written file, so no second refresh.
    #expect(try await home.store().current() == credentials)
    #expect(home.server.requests.count == 1)
  }

  /// The token endpoint's other answers: a 401 without a known code is a
  /// dead sign-in, a 200 that is not JSON and a cut connection are transient,
  /// and none of them names the refresh token or changes the file.
  @Test func otherTokenEndpointAnswersAreClassifiedAndRedacted() async throws {
    let home = try CodexHome()
    defer { home.stop() }
    try home.write(access: CodexHome.accessToken(expiresIn: 10))
    let before = try Data(contentsOf: home.file)

    home.server.enqueue(
      .json(
        ["error": "unauthorized", "error_description": "token rt_original rejected"], status: 401))
    let unauthorized = await #expect(throws: CodexCredentialError.self) {
      try await home.store().current()
    }
    guard case .signInExpired(let detail)? = unauthorized else {
      Issue.record("expected signInExpired, got \(String(describing: unauthorized))")
      return
    }
    #expect(detail == "unauthorized: HTTP 401: token [redacted] rejected")

    home.server.enqueue(Scripts.rawCompletion("<html>gateway</html>"))
    await #expect(throws: CodexCredentialError.refreshFailed("undecodable token response")) {
      try await home.store().current()
    }

    home.server.enqueue(.drop)
    let dropped = await #expect(throws: CodexCredentialError.self) {
      try await home.store().current()
    }
    guard case .refreshFailed(let message)? = dropped else {
      Issue.record("expected refreshFailed, got \(String(describing: dropped))")
      return
    }
    #expect(!message.contains("rt_original"))

    // A permanent code on an unexpected status is still permanent.
    home.server.enqueue(
      Scripts.tokenRefreshRejected(code: "refresh_token_invalidated", status: 403))
    let invalidated = await #expect(throws: CodexCredentialError.self) {
      try await home.store().current()
    }
    guard case .signInExpired? = invalidated else {
      Issue.record("expected signInExpired, got \(String(describing: invalidated))")
      return
    }
    #expect(try Data(contentsOf: home.file) == before, "a failed refresh never writes")
    #expect(home.server.requests.count == 4)
  }

  /// A reused-token answer when the file has not changed is final: one
  /// re-read, no second request.
  @Test func aReusedTokenWithAnUnchangedFileIsFinal() async throws {
    let home = try CodexHome()
    defer { home.stop() }
    try home.write(access: CodexHome.accessToken(expiresIn: 10))
    home.server.enqueue(Scripts.tokenRefreshRejected(code: "refresh_token_reused"))
    let error = await #expect(throws: CodexCredentialError.self) {
      try await home.store().current()
    }
    guard case .signInExpired(let detail)? = error else {
      Issue.record("expected signInExpired, got \(String(describing: error))")
      return
    }
    #expect(detail.contains("refresh_token_reused"))
    #expect(home.server.requests.count == 1)
  }

  @Test func jwtClaimsDecodeBase64URLWithoutPadding() {
    let token = CodexHome.idToken(email: "a@b.c", plan: "team")
    #expect(JWTClaims.email(of: token) == "a@b.c")
    #expect(JWTClaims.planType(of: token) == "team")
    #expect(JWTClaims.accountID(of: token) == "acct_jwt")
    #expect(JWTClaims.expiry(of: token) == nil)
    #expect(JWTClaims.payload(of: "not.a") == nil)
    #expect(JWTClaims.payload(of: "a.!!!.c") == nil)
  }
}

/// The findings of the 2026-09-30 review: one refresh for concurrent
/// callers, the token endpoint's nested error shape, the write-back over
/// the file's latest contents, and the 401 path that trusts a rotated file.
@Suite struct CodexCredentialStoreConcurrencyTests {
  /// Two chunks arriving while the token is inside its window must not both
  /// spend the same refresh token; the real endpoint rejects the second.
  @Test func concurrentCallersShareOneRefresh() async throws {
    let home = try CodexHome()
    defer { home.stop() }
    try home.write(access: CodexHome.accessToken(expiresIn: 10))
    let fresh = CodexHome.accessToken(expiresIn: 3_600)
    home.server.respond { request in
      request.index == 0
        ? Scripts.tokenRefresh(access: fresh, refresh: "rt_2")
        : Scripts.tokenRefreshRejected(code: "refresh_token_reused")
    }
    let store = home.store()
    async let first = store.current()
    async let second = store.current()
    async let third = store.refreshed(ifStillUsing: CodexHome.accessToken(expiresIn: 10))
    let (a, b, c) = try await (first, second, third)
    #expect(a == b)
    #expect(b == c)
    #expect(a.refreshToken == "rt_2")
    #expect(home.server.requests.count == 1)
    // Once done, the next caller reads the file and needs nothing.
    #expect(try await store.current().accessToken == fresh)
    #expect(home.server.requests.count == 1)
  }

  /// `{"error": {"code": …}}`, the shape the endpoint uses beside the flat
  /// one: a permanent code on a 400 is final, and a reused code triggers
  /// the re-read.
  @Test func nestedErrorCodesArePermanentAndTriggerTheReread() async throws {
    let home = try CodexHome()
    defer { home.stop() }
    try home.write(access: CodexHome.accessToken(expiresIn: 10))
    home.server.enqueue(
      StubResponse(
        status: 400, headers: ["Content-Type": "application/json"],
        body: Data(#"{"error":{"code":"refresh_token_expired","message":"gone"}}"#.utf8)))
    let error = await #expect(throws: CodexCredentialError.self) {
      try await home.store().current()
    }
    guard case .signInExpired(let detail)? = error else {
      Issue.record("expected signInExpired, got \(String(describing: error))")
      return
    }
    #expect(detail.hasPrefix("refresh_token_expired: HTTP 400: gone"))
    #expect(!String(describing: error!).contains("refresh_token"), "no wire code on screen")

    let second = try CodexHome()
    defer { second.stop() }
    try second.write(access: CodexHome.accessToken(expiresIn: 10))
    let fresh = CodexHome.accessToken(expiresIn: 3_600)
    second.server.respond { request in
      let body = (try? JSONDecoder().decode([String: String].self, from: request.body)) ?? [:]
      if body["refresh_token"] == "rt_original" {
        try? second.write(access: CodexHome.accessToken(expiresIn: 10), refresh: "rt_from_cli")
        return StubResponse(
          status: 400, headers: ["Content-Type": "application/json"],
          body: Data(#"{"error":{"code":"refresh_token_reused"}}"#.utf8))
      }
      return Scripts.tokenRefresh(access: fresh, refresh: "rt_after_cli")
    }
    #expect(try await second.store().current().refreshToken == "rt_after_cli")
    #expect(second.server.requests.count == 2)
  }

  /// A token the endpoint echoes inside its error code never reaches the
  /// detail either.
  @Test func errorCodesAreRedactedToo() async throws {
    let home = try CodexHome()
    defer { home.stop() }
    try home.write(access: CodexHome.accessToken(expiresIn: 10))
    home.server.enqueue(
      StubResponse(
        status: 401, headers: ["Content-Type": "application/json"],
        body: Data(#"{"error":"bad rt_original"}"#.utf8)))
    let error = await #expect(throws: CodexCredentialError.self) {
      try await home.store().current()
    }
    let detail = error?.detail ?? ""
    #expect(!detail.contains("rt_original"))
    #expect(detail.contains("[redacted]"))
  }

  /// What the CLI wrote during the round trip survives the write-back.
  @Test func writeBackOverlaysTheFilesLatestContents() async throws {
    let home = try CodexHome()
    defer { home.stop() }
    try home.write(access: CodexHome.accessToken(expiresIn: 10))
    let fresh = CodexHome.accessToken(expiresIn: 3_600)
    home.server.respond { _ in
      try? home.write(
        access: CodexHome.accessToken(expiresIn: 10),
        extra: ["written_by_cli": .string("yes"), "agent_identity": .object(["keep": .bool(true)])])
      return Scripts.tokenRefresh(access: fresh, refresh: "rt_2")
    }
    #expect(try await home.store().current().refreshToken == "rt_2")
    let document = try home.document()
    #expect(document["written_by_cli"] == .string("yes"))
    #expect(document["agent_identity"] == .object(["keep": .bool(true)]))
    guard case .object(let tokens)? = document["tokens"] else {
      Issue.record("tokens gone")
      return
    }
    #expect(tokens["access_token"] == .string(fresh))
  }

  /// A 401 after the CLI rotated the file: the file wins, no network.
  @Test func theUnauthorizedPathTrustsARotatedFile() async throws {
    let home = try CodexHome()
    defer { home.stop() }
    let rotated = CodexHome.accessToken(expiresIn: 3_600, plan: "pro")
    try home.write(access: rotated, refresh: "rt_cli")
    let credentials = try await home.store().refreshed(ifStillUsing: "stale-token")
    #expect(credentials.accessToken == rotated)
    #expect(credentials.refreshToken == "rt_cli")
    #expect(home.server.requests.isEmpty)
  }
}
