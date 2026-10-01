import AppKit
import Foundation
import StenoBridge
import StenoCore

// What every window host does with a request before its own routing: read
// the params as the method's contract type, wrap a reply value, and run the
// two system commands every page may call. Shared by `MainWindowBridge` and
// `SettingsBridge` so the error wording is one.

extension BridgeRequest {
  /// The method's params decoded into its contract type; a missing or
  /// unreadable value is `invalidParams`.
  func params<T: Decodable>(_ type: T.Type) throws -> T {
    guard let decoded = try optionalParams(type) else {
      throw BridgeError(code: .invalidParams, message: "\(method.rawValue) needs params.")
    }
    return decoded
  }

  /// The params when present; nil for a missing or `null` value.
  func optionalParams<T: Decodable>(_ type: T.Type) throws -> T? {
    guard let value = params, value != .null else { return nil }
    do {
      return try BridgeJSON.decode(T.self, from: BridgeJSON.encode(value))
    } catch {
      throw BridgeError(code: .invalidParams, message: "\(method.rawValue): \(error)")
    }
  }
}

enum BridgeReplies {
  /// A reply value as the dispatcher carries it.
  static func value(_ value: some Encodable) throws -> JSONValue {
    try BridgeJSON.decode(JSONValue.self, from: BridgeDispatcher.encoder().encode(value))
  }
}

/// The system commands a page may call: `system.openURL` and
/// `system.openSystemSettings`.
enum BridgeSystemCommands {
  /// Opens an `https:` or `mailto:` link in the default handler; anything
  /// else is `invalidParams`, so a page cannot open files or run scripts.
  @MainActor
  static func openURL(_ text: String) throws {
    guard let url = URL(string: text), let scheme = url.scheme?.lowercased(),
      scheme == "https" || scheme == "mailto"
    else {
      throw BridgeError(
        code: .invalidParams, message: "Only https: and mailto: links open from the page.")
    }
    NSWorkspace.shared.open(url)
  }

  /// The app's permission for a wire kind; the raw values are shared.
  static func permissionKind(_ kind: BridgePermissionKind) throws -> PermissionKind {
    guard let permission = PermissionKind(rawValue: kind.rawValue) else {
      throw BridgeError(code: .invalidParams, message: "Unknown permission \(kind.rawValue).")
    }
    return permission
  }
}
