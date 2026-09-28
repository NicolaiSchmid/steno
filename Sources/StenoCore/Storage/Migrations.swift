import Foundation
import GRDB

/// The only migration file. Append-only: a schema change is a new
/// `registerMigration` below the last one, never an edit above it, and
/// `SchemaSnapshotTests` keeps every version's `sqlite_master` dump
/// byte-identical. `eraseDatabaseOnSchemaChange` stays `false` everywhere.
///
/// Tables keep the implicit rowid (never `withoutRowID`) because the FTS5
/// external-content tables join on it; the app never runs `VACUUM`, and
/// `steno dev db reindex` rebuilds the index if it drifts.
public enum Migrations {
  public static func migrator() -> DatabaseMigrator {
    migrator(upTo: steps.count)
  }

  /// The migrator for the first `count` versions. `SchemaSnapshotTests` runs
  /// every prefix against its golden; production always runs them all.
  static func migrator(upTo count: Int) -> DatabaseMigrator {
    var migrator = DatabaseMigrator()
    for step in steps.prefix(count) {
      migrator.registerMigration(step.identifier, migrate: step.migrate)
    }
    return migrator
  }

  /// One schema version: its GRDB identifier and the migration.
  struct Step: Sendable {
    let identifier: String
    let migrate: @Sendable (Database) throws -> Void
  }

  /// Every version in registration order. Append below the last one.
  static let steps: [Step] = [
    Step(identifier: "v1", migrate: v1),
    Step(identifier: "v2", migrate: v2),
    Step(identifier: "v3", migrate: v3),
    Step(identifier: "v4", migrate: v4),
  ]

  /// Every identifier in registration order; tests compare it with what a
  /// database has applied.
  public static let identifiers = steps.map(\.identifier)

  /// Internal so `SchemaSnapshotTests` can run each version on its own.
  static func v1(_ db: Database) throws {
    try db.create(table: "meeting") { t in
      t.primaryKey("id", .text)
      t.column("title", .text).notNull()
      t.column("startedAt", .datetime).notNull()
      t.column("duration", .double).notNull()
      t.column("language", .text)
      t.column("source", .text).notNull()
      t.column("calendarEventID", .text)
      t.column("tags", .text).notNull().defaults(to: "[]")
      t.column("state", .text).notNull()
      t.column("failureReason", .text)
      t.column("templateID", .text).notNull()
      t.column("summary", .text)
      t.column("summaryText", .text).notNull().defaults(to: "")
      t.column("scratchpad", .text).notNull().defaults(to: "")
      t.column("llmUsage", .text)
      t.column("createdAt", .datetime).notNull()
      t.column("updatedAt", .datetime).notNull()
    }
    try db.create(index: "meeting_state", on: "meeting", columns: ["state"])
    try db.create(index: "meeting_startedAt", on: "meeting", columns: ["startedAt"])

    try db.create(table: "person") { t in
      t.primaryKey("id", .text)
      t.column("displayName", .text).notNull()
      t.column("email", .text)
      t.column("embedding", .blob)
      t.column("sampleCount", .integer).notNull().defaults(to: 0)
      t.column("createdAt", .datetime).notNull()
    }

    try db.create(table: "participant") { t in
      t.primaryKey("id", .text)
      t.column("meetingID", .text).notNull().references("meeting", onDelete: .cascade)
      t.column("personID", .text).references("person", onDelete: .setNull)
      t.column("displayName", .text).notNull()
      t.column("role", .text).notNull()
      t.column("email", .text)
    }
    try db.create(index: "participant_meetingID", on: "participant", columns: ["meetingID"])
    try db.create(index: "participant_personID", on: "participant", columns: ["personID"])

    try db.create(table: "speaker") { t in
      t.primaryKey("id", .text)
      t.column("meetingID", .text).notNull().references("meeting", onDelete: .cascade)
      t.column("clusterLabel", .text).notNull()
      t.column("assignment", .text).notNull()
      t.column("personID", .text).references("person", onDelete: .setNull)
      t.column("similarity", .double)
      t.column("embedding", .blob)
      t.column("sampleClipStart", .double)
      t.column("sampleClipEnd", .double)
      t.column("sampleClipURL", .text)
      t.column("clusterConfidence", .double).notNull()
    }
    try db.create(index: "speaker_meetingID", on: "speaker", columns: ["meetingID"])
    try db.create(index: "speaker_personID", on: "speaker", columns: ["personID"])

    try db.create(table: "transcriptSegment") { t in
      t.primaryKey("id", .text)
      t.column("meetingID", .text).notNull().references("meeting", onDelete: .cascade)
      t.column("start", .double).notNull()
      t.column("end", .double).notNull()
      t.column("speakerID", .text).references("speaker", onDelete: .setNull)
      t.column("lane", .text).notNull()
      t.column("text", .text).notNull()
      t.column("rawText", .text).notNull()
    }
    try db.create(
      index: "transcriptSegment_meetingID_start", on: "transcriptSegment",
      columns: ["meetingID", "start"])
    try db.create(
      index: "transcriptSegment_speakerID", on: "transcriptSegment", columns: ["speakerID"])

    try db.create(table: "meetingTask") { t in
      t.primaryKey("id", .text)
      t.column("meetingID", .text).notNull().references("meeting", onDelete: .cascade)
      t.column("text", .text).notNull()
      t.column("assigneePersonID", .text).references("person", onDelete: .setNull)
      t.column("assigneeName", .text)
      t.column("priority", .text).notNull()
      t.column("dueDate", .datetime)
      t.column("done", .boolean).notNull().defaults(to: false)
    }
    try db.create(index: "meetingTask_meetingID", on: "meetingTask", columns: ["meetingID"])

    try db.create(table: "decision") { t in
      t.primaryKey("id", .text)
      t.column("meetingID", .text).notNull().references("meeting", onDelete: .cascade)
      t.column("text", .text).notNull()
    }
    try db.create(index: "decision_meetingID", on: "decision", columns: ["meetingID"])

    try db.create(table: "audioAsset") { t in
      t.primaryKey("id", .text)
      t.column("meetingID", .text).notNull().references("meeting", onDelete: .cascade)
      t.column("url", .text).notNull()
      t.column("format", .text).notNull()
      t.column("lanes", .text).notNull()
      t.column("sidecars16k", .text).notNull().defaults(to: "{}")
      t.column("mixdownURL", .text)
      t.column("retention", .text).notNull()
      t.column("retentionDays", .integer)
      t.column("expiresAt", .datetime)
    }
    try db.create(index: "audioAsset_meetingID", on: "audioAsset", columns: ["meetingID"])
    try db.create(index: "audioAsset_expiresAt", on: "audioAsset", columns: ["expiresAt"])

    try db.create(table: "delivery") { t in
      t.primaryKey("id", .text)
      t.column("meetingID", .text).notNull().references("meeting", onDelete: .cascade)
      t.column("destinationID", .text).notNull()
      t.column("status", .text).notNull()
      t.column("failureMessage", .text)
      t.column("lastAttemptAt", .datetime)
      t.column("receipt", .text)
      t.uniqueKey(["meetingID", "destinationID"])
    }

    try db.create(table: "pairedDevice") { t in
      t.primaryKey("id", .text)
      t.column("name", .text).notNull()
      t.column("pairedAt", .datetime).notNull()
      t.column("lastSeenAt", .datetime)
      t.column("tokenHash", .blob).notNull().unique()
    }

    try db.create(table: "handoverReceipt") { t in
      t.primaryKey("recordingID", .text)
      t.column("deviceID", .text).notNull().references("pairedDevice", onDelete: .cascade)
      t.column("state", .text).notNull()
      t.column("meetingID", .text)
      t.column("failureMessage", .text)
      t.column("byteCount", .integer).notNull()
      t.column("sha256", .blob).notNull()
      t.column("chunkSize", .integer).notNull()
      t.column("receivedChunks", .text).notNull().defaults(to: "[]")
      t.column("createdAt", .datetime).notNull()
      t.column("updatedAt", .datetime).notNull()
    }
    try db.create(index: "handoverReceipt_deviceID", on: "handoverReceipt", columns: ["deviceID"])

    try db.create(table: "setting") { t in
      t.primaryKey("key", .text)
      t.column("value", .text).notNull()
    }

    try db.create(virtualTable: "transcriptSegment_ft", using: FTS5()) { t in
      t.synchronize(withTable: "transcriptSegment")
      t.tokenizer = .unicode61()
      t.column("text")
    }
    try db.create(virtualTable: "meeting_ft", using: FTS5()) { t in
      t.synchronize(withTable: "meeting")
      t.tokenizer = .unicode61()
      t.column("title")
      t.column("summaryText")
    }
  }

  /// The model's name guesses per speaker (#78): one row per speaker,
  /// written by the summarize stage, gone with the speaker or the meeting.
  static func v2(_ db: Database) throws {
    try db.create(table: "speakerNameSuggestion") { t in
      t.primaryKey("speakerID", .text).references("speaker", onDelete: .cascade)
      t.column("meetingID", .text).notNull().references("meeting", onDelete: .cascade)
      t.column("name", .text).notNull()
      t.column("confidence", .double).notNull()
      t.column("evidence", .text).notNull()
    }
    try db.create(
      index: "speakerNameSuggestion_meetingID", on: "speakerNameSuggestion",
      columns: ["meetingID"])
  }

  /// Why a recording ended and where its title came from (the 2026-09-28
  /// device-change plan): two columns on `meeting`, nullable or defaulted, so
  /// every row written before reads as "no reason recorded" and "default
  /// title". `meeting_ft` is unaffected: its triggers name their columns.
  static func v3(_ db: Database) throws {
    try db.alter(table: "meeting") { t in
      t.add(column: "endReason", .text)
      t.add(column: "titleOrigin", .text).notNull().defaults(to: "default")
    }
  }

  /// The learned per-stage rates behind `progress` estimates
  /// (`MeetingStore+Timings.swift`): one row per stage and key, the speech
  /// engine id for transcribe, the LLM model for cleanup and summarize,
  /// empty for the rest. Measurements, not choices, so not a `setting`.
  static func v4(_ db: Database) throws {
    try db.create(table: "stageRate") { t in
      t.column("stage", .text).notNull()
      t.column("key", .text).notNull()
      t.column("samples", .integer).notNull()
      t.column("secondsPerUnit", .double).notNull()
      t.column("updatedAt", .datetime).notNull()
      t.primaryKey(["stage", "key"])
    }
  }
}
