-- GRDB migration "v5": the handover admission ledger, one row per phone
-- recording the computer admitted: its id, size and SHA-256, and the meeting
-- it became. No foreign key: a revoke and a meeting delete leave it.
-- Mirrors "v5" in Sources/StenoCore/Storage/Migrations.swift; applied by
-- both sides until cutover; never edit once shipped.
CREATE TABLE "handoverAdmission" ("recordingID" TEXT NOT NULL, "byteCount" INTEGER NOT NULL, "sha256" BLOB NOT NULL, "meetingID" TEXT NOT NULL, "admittedAt" DATETIME NOT NULL, PRIMARY KEY ("recordingID", "byteCount", "sha256"));
