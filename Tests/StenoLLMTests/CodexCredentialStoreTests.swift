import Foundation
import StenoCore
import Testing

@testable import StenoLLM

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
      document["last_refresh"] = .string(CodexCredentialStore.formatDate(lastRefresh))
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
    let credentials = try await home.store().status()
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
    #expect(try await home.store().status().accountID == "acct_jwt")
  }

  @Test func missingFileAPIKeyLoginAndMalformedFilesAreTold() async throws {
    let home = try CodexHome()
    defer { home.stop() }
    await #expect(throws: CodexCredentialError.notSignedIn) { try await home.store().status() }

    try Data(#"{"OPENAI_API_KEY":"sk-x","tokens":null}"#.utf8).write(to: home.file)
    await #expect(throws: CodexCredentialError.apiKeyLogin) { try await home.store().status() }

    try home.write(authMode: "apikey")
    await #expect(throws: CodexCredentialError.apiKeyLogin) { try await home.store().status() }

    try Data("not json".utf8).write(to: home.file)
    let error = await #expect(throws: CodexCredentialError.self) {
      try await home.store().status()
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
    #expect(try await home.store().status().refreshToken == "rt_original")
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

  @Test func defaultHomeHonoursCodexHome() {
    #expect(
      CodexCredentialStore.defaultHome(environment: ["CODEX_HOME": "/tmp/elsewhere"]).path
        == "/tmp/elsewhere")
    #expect(CodexCredentialStore.defaultHome(environment: [:]).lastPathComponent == ".codex")
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
