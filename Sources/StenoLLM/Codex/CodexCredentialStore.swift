import Foundation
import StenoCore

#if canImport(FoundationNetworking)
  import FoundationNetworking
#endif
#if canImport(Glibc)
  import Glibc
#elseif canImport(Darwin)
  import Darwin
#endif

/// The ChatGPT sign-in the Codex CLI stored, as far as Steno reads it.
/// Never persisted anywhere but the CLI's own file; never logged.
public struct CodexCredentials: Sendable, Equatable {
  public var accessToken: String
  public var refreshToken: String
  public var accountID: String
  public var email: String?
  /// "plus", "pro", "free", "business", …; nil when the token does not say.
  public var planType: String?
  /// The access token's `exp` claim; nil when the token carries none.
  public var expiresAt: Date?
  /// The file's `last_refresh`; nil for a file the CLI never refreshed.
  public var lastRefresh: Date?

  /// "name@example.com (Plus)", "name@example.com", or "your ChatGPT account".
  public var accountLine: String {
    let plan = planType.map { $0.prefix(1).uppercased() + $0.dropFirst() }
    switch (email, plan) {
    case (let email?, let plan?): return "\(email) (\(plan))"
    case (let email?, nil): return email
    case (nil, let plan?): return "your ChatGPT account (\(plan))"
    case (nil, nil): return "your ChatGPT account"
    }
  }
}

public enum CodexCredentialError: Error, Sendable, Equatable, CustomStringConvertible {
  /// No `auth.json`, or one without ChatGPT tokens.
  case notSignedIn
  /// The file holds an API key login, which the Codex backend does not
  /// take; the endpoint provider with the OpenAI preset is the way.
  case apiKeyLogin
  case malformed(String)
  /// The refresh token is spent, expired or revoked: only `codex login`
  /// helps.
  case signInExpired(String)
  /// The refresh did not go through for a reason a retry may fix.
  case refreshFailed(String)

  public var description: String {
    switch self {
    case .notSignedIn:
      "No Codex sign-in found. Run `codex login` in Terminal, then try again."
    case .apiKeyLogin:
      "Codex is signed in with an API key, not a ChatGPT account. Use the server or API key option instead."
    case .malformed(let detail):
      "The Codex sign-in file could not be read: \(detail)"
    case .signInExpired(let detail):
      "The Codex sign-in has expired. Run `codex login` in Terminal, then try again. (\(detail))"
    case .refreshFailed(let detail):
      "The Codex sign-in could not be refreshed: \(detail)"
    }
  }
}

/// Reads and refreshes `$CODEX_HOME/auth.json` the way the Codex CLI does,
/// so the two stay signed in together. Every read goes back to the file
/// (the CLI may have rotated the tokens meanwhile); a refresh is written
/// back atomically with every unknown key preserved, because refresh tokens
/// rotate and the CLI would otherwise be signed out. Callers never see the
/// file: they get `CodexCredentials` and attach the access token themselves.
public actor CodexCredentialStore {
  /// `CODEX_HOME`, else `~/.codex`.
  public static func defaultHome(
    environment: [String: String] = ProcessInfo.processInfo.environment
  )
    -> URL
  {
    if let home = environment["CODEX_HOME"], !home.isEmpty {
      return URL(fileURLWithPath: home, isDirectory: true)
    }
    return FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(
      ".codex", isDirectory: true)
  }

  /// The OAuth client the Codex CLI refreshes with; the token endpoint
  /// accepts refresh tokens for it only.
  public static let codexClientID = "app_EMoamEEZ73f0CkXaXp7hrann"
  public static let defaultTokenEndpoint = URL(string: "https://auth.openai.com/oauth/token")!
  /// The CLI refreshes an access token this close to its `exp`.
  static let expiryWindow: TimeInterval = 5 * 60
  /// The CLI refreshes a file whose `last_refresh` is older than this even
  /// when the access token has not expired.
  static let staleAfter: TimeInterval = 8 * 24 * 60 * 60

  public let home: URL
  private let session: URLSession
  private let tokenEndpoint: URL
  private let clientID: String
  private let now: @Sendable () -> Date

  public init(
    home: URL = CodexCredentialStore.defaultHome(),
    session: URLSession = .shared,
    tokenEndpoint: URL = CodexCredentialStore.defaultTokenEndpoint,
    clientID: String = CodexCredentialStore.codexClientID,
    now: @escaping @Sendable () -> Date = { Date() }
  ) {
    self.home = home
    self.session = session
    self.tokenEndpoint = tokenEndpoint
    self.clientID = clientID
    self.now = now
  }

  public var fileURL: URL { home.appendingPathComponent("auth.json") }

  /// What the file says, without touching the network. For the consent
  /// card and the status row.
  public func status() throws -> CodexCredentials {
    try read().credentials
  }

  /// Credentials fit to send: the file's tokens, refreshed first when the
  /// access token is within `expiryWindow` of its expiry or the file is
  /// older than `staleAfter`.
  public func current() async throws -> CodexCredentials {
    let file = try read()
    guard needsRefresh(file.credentials) else { return file.credentials }
    return try await refresh(file)
  }

  /// A refresh regardless of age, for the 401 path.
  public func refreshed() async throws -> CodexCredentials {
    try await refresh(try read())
  }

  func needsRefresh(_ credentials: CodexCredentials) -> Bool {
    let now = self.now()
    if let expiresAt = credentials.expiresAt,
      expiresAt.timeIntervalSince(now) < Self.expiryWindow
    {
      return true
    }
    if let lastRefresh = credentials.lastRefresh,
      now.timeIntervalSince(lastRefresh) > Self.staleAfter
    {
      return true
    }
    return false
  }

  // MARK: File

  struct AuthFile: Sendable {
    /// The whole document, kept so unknown keys survive a write-back.
    var document: [String: JSONValue]
    var credentials: CodexCredentials
  }

  func read() throws -> AuthFile {
    let data: Data
    do {
      data = try Data(contentsOf: fileURL)
    } catch {
      if !FileManager.default.fileExists(atPath: fileURL.path) {
        throw CodexCredentialError.notSignedIn
      }
      throw CodexCredentialError.malformed(String(describing: error))
    }
    guard let value = try? JSONDecoder().decode(JSONValue.self, from: data),
      case .object(let document) = value
    else {
      throw CodexCredentialError.malformed("not a JSON object")
    }
    return AuthFile(document: document, credentials: try Self.credentials(in: document))
  }

  static func credentials(in document: [String: JSONValue]) throws -> CodexCredentials {
    guard case .object(let tokens)? = document["tokens"] else {
      if case .string(let key)? = document["OPENAI_API_KEY"], !key.isEmpty {
        throw CodexCredentialError.apiKeyLogin
      }
      throw CodexCredentialError.notSignedIn
    }
    if case .string(let mode)? = document["auth_mode"], mode.lowercased() != "chatgpt" {
      throw CodexCredentialError.apiKeyLogin
    }
    guard case .string(let accessToken)? = tokens["access_token"], !accessToken.isEmpty else {
      throw CodexCredentialError.malformed("no access token")
    }
    guard case .string(let refreshToken)? = tokens["refresh_token"], !refreshToken.isEmpty else {
      throw CodexCredentialError.malformed("no refresh token")
    }
    let idToken: String? =
      if case .string(let token)? = tokens["id_token"] { token } else { nil }
    var accountID: String?
    if case .string(let stored)? = tokens["account_id"], !stored.isEmpty {
      accountID = stored
    } else if let idToken {
      accountID = JWTClaims.accountID(of: idToken)
    }
    guard let accountID else {
      throw CodexCredentialError.malformed("no account id")
    }
    var lastRefresh: Date?
    if case .string(let text)? = document["last_refresh"] {
      // RFC 3339 with or without fractional seconds, as chrono writes it.
      lastRefresh = StenoJSON.parse(text)
    }
    return CodexCredentials(
      accessToken: accessToken,
      refreshToken: refreshToken,
      accountID: accountID,
      email: idToken.flatMap(JWTClaims.email(of:)) ?? JWTClaims.email(of: accessToken),
      planType: idToken.flatMap(JWTClaims.planType(of:)) ?? JWTClaims.planType(of: accessToken),
      expiresAt: JWTClaims.expiry(of: accessToken),
      lastRefresh: lastRefresh)
  }

  // MARK: Refresh

  private struct RefreshResponse: Decodable {
    var idToken: String?
    var accessToken: String?
    var refreshToken: String?

    enum CodingKeys: String, CodingKey {
      case idToken = "id_token"
      case accessToken = "access_token"
      case refreshToken = "refresh_token"
    }
  }

  private struct RefreshError: Decodable {
    var error: String?
    var errorDescription: String?
    var errorCode: String?

    enum CodingKeys: String, CodingKey {
      case error
      case errorDescription = "error_description"
      case errorCode = "error_code"
    }
  }

  /// The error codes the token endpoint uses for a refresh token that will
  /// never work again.
  static let permanentRefreshCodes: Set<String> = [
    "refresh_token_expired", "refresh_token_reused", "refresh_token_invalidated", "invalid_grant",
  ]

  private func refresh(_ file: AuthFile) async throws -> CodexCredentials {
    do {
      return try await refreshOnce(file)
    } catch CodexCredentialError.signInExpired(let detail) where detail.contains("reused") {
      // The CLI may have rotated the token since our read; its file is the
      // truth. One more read, one more try, then the failure stands.
      let latest = try read()
      guard latest.credentials.refreshToken != file.credentials.refreshToken else {
        throw CodexCredentialError.signInExpired(detail)
      }
      return try await refreshOnce(latest)
    }
  }

  private func refreshOnce(_ file: AuthFile) async throws -> CodexCredentials {
    var request = URLRequest(url: tokenEndpoint)
    request.httpMethod = "POST"
    request.setValue("application/json", forHTTPHeaderField: "Content-Type")
    request.setValue("application/json", forHTTPHeaderField: "Accept")
    request.setValue("steno/\(StenoCore.version)", forHTTPHeaderField: "User-Agent")
    request.timeoutInterval = 30
    let body: [String: String] = [
      "grant_type": "refresh_token",
      "client_id": clientID,
      "refresh_token": file.credentials.refreshToken,
    ]
    request.httpBody = try JSONEncoder().encode(body)
    let data: Data
    let response: URLResponse
    do {
      (data, response) = try await session.data(for: request)
    } catch {
      throw CodexCredentialError.refreshFailed(
        LLMTransport.redact(String(describing: error), secrets: [file.credentials.refreshToken]))
    }
    guard let http = response as? HTTPURLResponse else {
      throw CodexCredentialError.refreshFailed("not an HTTP response")
    }
    guard (200..<300).contains(http.statusCode) else {
      let detail = try? JSONDecoder().decode(RefreshError.self, from: data)
      let code = (detail?.errorCode ?? detail?.error ?? "").lowercased()
      let message = LLMTransport.redact(
        "HTTP \(http.statusCode): \(detail?.errorDescription ?? detail?.error ?? String(decoding: data.prefix(300), as: UTF8.self))",
        secrets: [file.credentials.refreshToken])
      if http.statusCode == 401 || Self.permanentRefreshCodes.contains(code) {
        throw CodexCredentialError.signInExpired(code.isEmpty ? message : "\(code): \(message)")
      }
      throw CodexCredentialError.refreshFailed(message)
    }
    guard let refreshed = try? JSONDecoder().decode(RefreshResponse.self, from: data) else {
      throw CodexCredentialError.refreshFailed("undecodable token response")
    }
    var document = file.document
    var tokens: [String: JSONValue] =
      if case .object(let existing)? = document["tokens"] { existing } else { [:] }
    if let accessToken = refreshed.accessToken, !accessToken.isEmpty {
      tokens["access_token"] = .string(accessToken)
    }
    if let refreshToken = refreshed.refreshToken, !refreshToken.isEmpty {
      tokens["refresh_token"] = .string(refreshToken)
    }
    if let idToken = refreshed.idToken, !idToken.isEmpty {
      tokens["id_token"] = .string(idToken)
    }
    document["tokens"] = .object(tokens)
    document["last_refresh"] = .string(StenoJSON.format(now()))
    try write(document)
    return try Self.credentials(in: document)
  }

  /// Temp file beside the target with mode 0600, then `rename`: readers see
  /// the old or the new file, never a partial one, and the mode never opens
  /// up on the way.
  private func write(_ document: [String: JSONValue]) throws {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
    let data = try encoder.encode(JSONValue.object(document))
    let temporary = home.appendingPathComponent(".auth.json.steno-\(UUID().uuidString)")
    guard
      FileManager.default.createFile(
        atPath: temporary.path, contents: data, attributes: [.posixPermissions: 0o600])
    else {
      throw CodexCredentialError.refreshFailed("could not write the sign-in file")
    }
    guard rename(temporary.path, fileURL.path) == 0 else {
      let code = errno
      try? FileManager.default.removeItem(at: temporary)
      throw CodexCredentialError.refreshFailed(
        "could not replace the sign-in file: \(String(cString: strerror(code)))")
    }
  }
}
