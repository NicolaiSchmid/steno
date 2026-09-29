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

/// Why the sign-in could not be used. `description` is the sentence the
/// user reads; `detail` is the technical text for logs and "Details".
public enum CodexCredentialError: Error, Sendable, Equatable, CustomStringConvertible {
  /// No `auth.json`, or one without ChatGPT tokens.
  case notSignedIn
  /// The file holds an API key login, which the Codex backend does not
  /// take; the OpenAI preset of the endpoint provider is the way.
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
      "Codex is signed in with an API key, not a ChatGPT account. Pick OpenAI as the service and paste that key instead."
    case .malformed:
      "The Codex sign-in file could not be read."
    case .signInExpired:
      "The Codex sign-in has expired. Run `codex login` in Terminal, then try again."
    case .refreshFailed:
      "The Codex sign-in could not be refreshed. Check the connection and try again."
    }
  }

  /// The technical reason, already redacted; nil for the two cases that
  /// have none.
  public var detail: String? {
    switch self {
    case .notSignedIn, .apiKeyLogin: nil
    case .malformed(let detail), .signInExpired(let detail), .refreshFailed(let detail): detail
    }
  }
}

/// Reads and refreshes `$CODEX_HOME/auth.json` the way the Codex CLI does,
/// so the two stay signed in together. Every read goes back to the file
/// (the CLI may have rotated the tokens meanwhile); a refresh is written
/// back atomically over the file's latest contents with every unknown key
/// preserved, because refresh tokens rotate and the CLI would otherwise be
/// signed out. Concurrent callers share one refresh: the actor is
/// reentrant, and two refreshes with the same token would spend it twice.
/// Callers never see the file: they get `CodexCredentials` and attach the
/// access token themselves.
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
  /// The refresh in progress, awaited by every caller that arrives while
  /// it runs.
  private var refreshInFlight: Task<CodexCredentials, any Error>?

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
  public func stored() throws -> CodexCredentials {
    try read().credentials
  }

  /// Credentials fit to send: the file's tokens, refreshed first when the
  /// access token is within `expiryWindow` of its expiry or the file is
  /// older than `staleAfter`.
  public func current() async throws -> CodexCredentials {
    if let refreshInFlight { return try await refreshInFlight.value }
    let file = try read()
    guard needsRefresh(file.credentials) else { return file.credentials }
    return try await refresh(file)
  }

  /// The 401 path: the file's credentials when they have changed since
  /// `accessToken` was read (the CLI rotated them; no network), else one
  /// refresh regardless of age.
  public func refreshed(ifStillUsing accessToken: String) async throws -> CodexCredentials {
    if let refreshInFlight { return try await refreshInFlight.value }
    let file = try read()
    guard file.credentials.accessToken == accessToken else { return file.credentials }
    return try await refresh(file)
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

  /// The token endpoint's rejection, in the two shapes it uses:
  /// `{"error": {"code": …}}` and `{"error": "invalid_grant", "error_code":
  /// …, "error_description": …}`.
  private struct RefreshError: Decodable {
    var error: JSONValue?
    var errorDescription: String?
    var errorCode: String?

    enum CodingKeys: String, CodingKey {
      case error
      case errorDescription = "error_description"
      case errorCode = "error_code"
    }

    var code: String? {
      if let errorCode, !errorCode.isEmpty { return errorCode.lowercased() }
      switch error {
      case .string(let code)?: return code.isEmpty ? nil : code.lowercased()
      case .object(let object)?:
        if case .string(let code)? = object["code"], !code.isEmpty { return code.lowercased() }
        return nil
      default: return nil
      }
    }

    var message: String? {
      if let errorDescription, !errorDescription.isEmpty { return errorDescription }
      if case .object(let object)? = error, case .string(let text)? = object["message"] {
        return text
      }
      return nil
    }
  }

  /// A refresh the token endpoint turned down, before it is mapped to the
  /// public error: `refresh()` needs the code to decide on a re-read.
  private struct RefreshRejected: Error {
    var code: String?
    var permanent: Bool
    /// Already redacted.
    var message: String
  }

  /// The error codes the token endpoint uses for a refresh token that will
  /// never work again.
  static let permanentRefreshCodes: Set<String> = [
    "refresh_token_expired", "refresh_token_reused", "refresh_token_invalidated", "invalid_grant",
  ]

  /// One refresh shared by everyone who needs it while it runs.
  private func refresh(_ file: AuthFile) async throws -> CodexCredentials {
    if let refreshInFlight { return try await refreshInFlight.value }
    let task = Task { try await self.refreshRereadingOnReuse(file) }
    refreshInFlight = task
    defer { refreshInFlight = nil }
    return try await task.value
  }

  private func refreshRereadingOnReuse(_ file: AuthFile) async throws -> CodexCredentials {
    do {
      return try await refreshOnce(file)
    } catch let rejected as RefreshRejected {
      if rejected.code == "refresh_token_reused" {
        // The CLI may have rotated the token since our read; its file is the
        // truth. One more read, one more try, then the failure stands.
        let latest = try read()
        if latest.credentials.refreshToken != file.credentials.refreshToken {
          do {
            return try await refreshOnce(latest)
          } catch let again as RefreshRejected {
            throw Self.error(for: again)
          }
        }
      }
      throw Self.error(for: rejected)
    }
  }

  private static func error(for rejected: RefreshRejected) -> CodexCredentialError {
    let detail = rejected.code.map { "\($0): \(rejected.message)" } ?? rejected.message
    return rejected.permanent ? .signInExpired(detail) : .refreshFailed(detail)
  }

  private func refreshOnce(_ file: AuthFile) async throws -> CodexCredentials {
    let secrets = [file.credentials.refreshToken, file.credentials.accessToken]
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
        LLMTransport.redact(String(describing: error), secrets: secrets))
    }
    guard let http = response as? HTTPURLResponse else {
      throw CodexCredentialError.refreshFailed("not an HTTP response")
    }
    guard (200..<300).contains(http.statusCode) else {
      let rejection = try? JSONDecoder().decode(RefreshError.self, from: data)
      let code = rejection?.code.map { LLMTransport.redact($0, secrets: secrets) }
      let message = LLMTransport.redact(
        "HTTP \(http.statusCode): \(rejection?.message ?? String(decoding: data.prefix(300), as: UTF8.self))",
        secrets: secrets)
      throw RefreshRejected(
        code: code,
        permanent: http.statusCode == 401 || code.map(Self.permanentRefreshCodes.contains) ?? false,
        message: message)
    }
    guard let refreshed = try? JSONDecoder().decode(RefreshResponse.self, from: data) else {
      throw CodexCredentialError.refreshFailed("undecodable token response")
    }
    // Overlay the new tokens on what the file holds now, not on the copy
    // read before the round trip: the CLI may have written other keys
    // meanwhile, and those must survive.
    var document = (try? read().document) ?? file.document
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
