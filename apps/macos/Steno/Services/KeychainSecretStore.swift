import Foundation
import Security
import StenoCore

/// StenoCore's `SecretStore` over the login keychain: one generic password
/// per `SecretKey`, service `uno.schmid.steno.mac` (or the label a test
/// passes), account the key's raw value. `Settings` never carries the API
/// key; this is the only place it is stored.
struct KeychainSecretStore: SecretStore, Sendable {
  struct Failure: Error, CustomStringConvertible, Sendable {
    var call: String
    var status: OSStatus
    var description: String {
      let message = SecCopyErrorMessageString(status, nil) as String? ?? "OSStatus \(status)"
      return "\(call) failed: \(message)"
    }
  }

  static let defaultService = "uno.schmid.steno.mac"
  let service: String

  init(service: String = KeychainSecretStore.defaultService) {
    self.service = service
  }

  func secret(for key: SecretKey) async throws -> String? {
    var query = baseQuery(for: key)
    query[kSecReturnData as String] = true
    query[kSecMatchLimit as String] = kSecMatchLimitOne
    var item: CFTypeRef?
    let status = SecItemCopyMatching(query as CFDictionary, &item)
    switch status {
    case errSecSuccess:
      guard let data = item as? Data else { return nil }
      return String(data: data, encoding: .utf8)
    case errSecItemNotFound:
      return nil
    default:
      throw Failure(call: "SecItemCopyMatching", status: status)
    }
  }

  func setSecret(_ value: String?, for key: SecretKey) async throws {
    let query = baseQuery(for: key)
    guard let value, !value.isEmpty else {
      let status = SecItemDelete(query as CFDictionary)
      guard status == errSecSuccess || status == errSecItemNotFound else {
        throw Failure(call: "SecItemDelete", status: status)
      }
      return
    }
    let data = Data(value.utf8)
    let update: [String: Any] = [kSecValueData as String: data]
    let updateStatus = SecItemUpdate(query as CFDictionary, update as CFDictionary)
    switch updateStatus {
    case errSecSuccess:
      return
    case errSecItemNotFound:
      var insert = query
      insert[kSecValueData as String] = data
      insert[kSecAttrLabel as String] = "Steno \(key.rawValue)"
      let status = SecItemAdd(insert as CFDictionary, nil)
      guard status == errSecSuccess else { throw Failure(call: "SecItemAdd", status: status) }
    default:
      throw Failure(call: "SecItemUpdate", status: updateStatus)
    }
  }

  private func baseQuery(for key: SecretKey) -> [String: Any] {
    [
      kSecClass as String: kSecClassGenericPassword,
      kSecAttrService as String: service,
      kSecAttrAccount as String: key.rawValue,
    ]
  }
}
