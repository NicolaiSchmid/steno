import Foundation
import GRDB

/// Persists `Settings` as one row per property in the `setting` table, each
/// value a JSON fragment in the `StenoJSON` convention. A property missing
/// from the table loads as its default and an unknown row is ignored, so a
/// property can be added without a migration.
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
    writer.stream(
      ValueObservation.tracking { db in try Self.settings(from: try SettingRow.fetchAll(db)) })
  }

  static func rows(for settings: Settings) throws -> [SettingRow] {
    let object = try Self.object(settings)
    return try object.keys.sorted().map { key in
      let fragment = try StenoJSON.columnEncoder().encode(object[key])
      return SettingRow(key: key, value: String(decoding: fragment, as: UTF8.self))
    }
  }

  /// The defaults overlaid with every stored row; unknown rows fall away when
  /// `Settings` decodes.
  static func settings(from rows: [SettingRow]) throws -> Settings {
    var merged = try object(Settings())
    for row in rows {
      merged[row.key] = try StenoJSON.decode(JSONValue.self, from: Data(row.value.utf8))
    }
    return try StenoJSON.decode(Settings.self, from: StenoJSON.encode(JSONValue.object(merged)))
  }

  /// `settings` as a JSON object in the `StenoJSON` convention.
  private static func object(_ settings: Settings) throws -> [String: JSONValue] {
    guard
      case .object(let object) = try StenoJSON.decode(
        JSONValue.self, from: StenoJSON.encode(settings))
    else {
      throw DecodingError.dataCorrupted(
        DecodingError.Context(codingPath: [], debugDescription: "Settings is not a JSON object"))
    }
    return object
  }
}
