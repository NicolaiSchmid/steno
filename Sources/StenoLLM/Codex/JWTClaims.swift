import Foundation
import StenoCore

/// The payload of a JSON Web Token as the Codex sign-in stores them: the
/// middle segment, base64url without padding, a JSON object. Nothing here
/// verifies a signature; Steno only reads the claims the Codex CLI itself
/// reads (`exp`, `email`, the account and plan under the OpenAI auth claim)
/// and the server stays the judge of the token.
enum JWTClaims {
  static let openAIAuthClaim = "https://api.openai.com/auth"
  static let openAIProfileClaim = "https://api.openai.com/profile"

  /// The decoded payload, or nil when `token` is not three dot-separated
  /// segments whose middle one decodes to a JSON object.
  static func payload(of token: String) -> JSONValue? {
    let parts = token.split(separator: ".", omittingEmptySubsequences: false)
    guard parts.count == 3, !parts[1].isEmpty else { return nil }
    guard let data = base64URLDecode(String(parts[1])),
      let value = try? JSONDecoder().decode(JSONValue.self, from: data),
      case .object = value
    else { return nil }
    return value
  }

  /// `exp` as a date; nil when absent or not a number.
  static func expiry(of token: String) -> Date? {
    guard let payload = payload(of: token), case .number(let seconds)? = payload["exp"] else {
      return nil
    }
    return Date(timeIntervalSince1970: seconds)
  }

  /// `email`, else the profile claim's `email`.
  static func email(of token: String) -> String? {
    guard let payload = payload(of: token) else { return nil }
    if case .string(let email)? = payload["email"] { return email }
    if case .string(let email)? = payload[openAIProfileClaim]?["email"] { return email }
    return nil
  }

  /// `chatgpt_account_id` under the OpenAI auth claim.
  static func accountID(of token: String) -> String? {
    string(payload(of: token)?[openAIAuthClaim]?["chatgpt_account_id"])
  }

  /// `chatgpt_plan_type` under the OpenAI auth claim: "free", "plus", "pro",
  /// "business", "enterprise", "edu" or whatever the server adds next.
  static func planType(of token: String) -> String? {
    string(payload(of: token)?[openAIAuthClaim]?["chatgpt_plan_type"])
  }

  private static func string(_ value: JSONValue?) -> String? {
    if case .string(let text)? = value, !text.isEmpty { return text }
    return nil
  }

  static func base64URLDecode(_ text: String) -> Data? {
    var base64 = text.replacingOccurrences(of: "-", with: "+").replacingOccurrences(
      of: "_", with: "/")
    let remainder = base64.count % 4
    if remainder != 0 {
      base64 += String(repeating: "=", count: 4 - remainder)
    }
    return Data(base64Encoded: base64)
  }
}
