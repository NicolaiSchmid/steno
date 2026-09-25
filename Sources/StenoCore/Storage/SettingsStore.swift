import Foundation
import GRDB

/// Persists `Settings` as one row per property in the `setting` table, each
/// value a JSON fragment in the `StenoJSON` convention. A property missing
/// from the table decodes as its default, so adding one needs no migration.
public final class SettingsStore: Sendable {
  public let writer: any DatabaseWriter

  public init(writer: any DatabaseWriter) {
    self.writer = writer
  }

  public func load() async throws -> Settings {
    try await writer.read { db in try Self.settings(from: try SettingRow.fetchAll(db)) }
  }

  /// Writes every property, removing rows for properties that are now nil.
  public func save(_ settings: Settings) async throws {
    let rows = try Self.rows(for: settings)
    try await writer.write { db in
      try SettingRow.deleteAll(db)
      for row in rows { try row.insert(db) }
    }
  }

  /// The current settings, then again after every save.
  public func observe() -> AsyncThrowingStream<Settings, any Error> {
    observationStream(
      ValueObservation.tracking { db in try Self.settings(from: try SettingRow.fetchAll(db)) },
      in: writer)
  }

  static func rows(for settings: Settings) throws -> [SettingRow] {
    let data = try StenoJSON.columnEncoder().encode(settings)
    guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
      return []
    }
    return try object.keys.sorted().map { key in
      let fragment = try JSONSerialization.data(
        withJSONObject: object[key] as Any,
        options: [.fragmentsAllowed, .sortedKeys, .withoutEscapingSlashes])
      return SettingRow(key: key, value: String(decoding: fragment, as: UTF8.self))
    }
  }

  static func settings(from rows: [SettingRow]) throws -> Settings {
    let known = Set(Settings.CodingKeys.allCases.map(\.stringValue))
    let members =
      rows
      .filter { known.contains($0.key) }
      .sorted { $0.key < $1.key }
      .map { "\"\($0.key)\":\($0.value)" }
    let json = "{" + members.joined(separator: ",") + "}"
    return try StenoJSON.decode(Settings.self, from: Data(json.utf8))
  }
}
