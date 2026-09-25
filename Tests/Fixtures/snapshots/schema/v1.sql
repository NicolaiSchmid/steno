CREATE TRIGGER "__meeting_ft_ad" AFTER DELETE ON "meeting" BEGIN
    INSERT INTO "meeting_ft"("meeting_ft", "rowid", "title", "summaryText") VALUES('delete', old."rowid", old."title", old."summaryText");
END;

CREATE TRIGGER "__meeting_ft_ai" AFTER INSERT ON "meeting" BEGIN
    INSERT INTO "meeting_ft"("rowid", "title", "summaryText") VALUES (new."rowid", new."title", new."summaryText");
END;

CREATE TRIGGER "__meeting_ft_au" AFTER UPDATE ON "meeting" BEGIN
    INSERT INTO "meeting_ft"("meeting_ft", "rowid", "title", "summaryText") VALUES('delete', old."rowid", old."title", old."summaryText");
    INSERT INTO "meeting_ft"("rowid", "title", "summaryText") VALUES (new."rowid", new."title", new."summaryText");
END;

CREATE TRIGGER "__transcriptSegment_ft_ad" AFTER DELETE ON "transcriptSegment" BEGIN
    INSERT INTO "transcriptSegment_ft"("transcriptSegment_ft", "rowid", "text") VALUES('delete', old."rowid", old."text");
END;

CREATE TRIGGER "__transcriptSegment_ft_ai" AFTER INSERT ON "transcriptSegment" BEGIN
    INSERT INTO "transcriptSegment_ft"("rowid", "text") VALUES (new."rowid", new."text");
END;

CREATE TRIGGER "__transcriptSegment_ft_au" AFTER UPDATE ON "transcriptSegment" BEGIN
    INSERT INTO "transcriptSegment_ft"("transcriptSegment_ft", "rowid", "text") VALUES('delete', old."rowid", old."text");
    INSERT INTO "transcriptSegment_ft"("rowid", "text") VALUES (new."rowid", new."text");
END;

CREATE TABLE "audioAsset" ("id" TEXT PRIMARY KEY NOT NULL, "meetingID" TEXT NOT NULL REFERENCES "meeting"("id") ON DELETE CASCADE, "url" TEXT NOT NULL, "format" TEXT NOT NULL, "lanes" TEXT NOT NULL, "sidecars16k" TEXT NOT NULL DEFAULT '{}', "mixdownURL" TEXT, "retention" TEXT NOT NULL, "retentionDays" INTEGER, "expiresAt" DATETIME);

CREATE INDEX "audioAsset_expiresAt" ON "audioAsset"("expiresAt");

CREATE INDEX "audioAsset_meetingID" ON "audioAsset"("meetingID");

CREATE TABLE "decision" ("id" TEXT PRIMARY KEY NOT NULL, "meetingID" TEXT NOT NULL REFERENCES "meeting"("id") ON DELETE CASCADE, "text" TEXT NOT NULL);

CREATE INDEX "decision_meetingID" ON "decision"("meetingID");

CREATE TABLE "delivery" ("id" TEXT PRIMARY KEY NOT NULL, "meetingID" TEXT NOT NULL REFERENCES "meeting"("id") ON DELETE CASCADE, "destinationID" TEXT NOT NULL, "status" TEXT NOT NULL, "failureMessage" TEXT, "lastAttemptAt" DATETIME, "receipt" TEXT, UNIQUE ("meetingID", "destinationID"));

CREATE TABLE grdb_migrations (identifier TEXT NOT NULL PRIMARY KEY);

CREATE TABLE "handoverReceipt" ("recordingID" TEXT PRIMARY KEY NOT NULL, "deviceID" TEXT NOT NULL REFERENCES "pairedDevice"("id") ON DELETE CASCADE, "state" TEXT NOT NULL, "meetingID" TEXT, "failureMessage" TEXT, "byteCount" INTEGER NOT NULL, "sha256" BLOB NOT NULL, "chunkSize" INTEGER NOT NULL, "receivedChunks" TEXT NOT NULL DEFAULT '[]', "createdAt" DATETIME NOT NULL, "updatedAt" DATETIME NOT NULL);

CREATE INDEX "handoverReceipt_deviceID" ON "handoverReceipt"("deviceID");

CREATE TABLE "meeting" ("id" TEXT PRIMARY KEY NOT NULL, "title" TEXT NOT NULL, "startedAt" DATETIME NOT NULL, "duration" DOUBLE NOT NULL, "language" TEXT, "source" TEXT NOT NULL, "calendarEventID" TEXT, "tags" TEXT NOT NULL DEFAULT '[]', "state" TEXT NOT NULL, "failureReason" TEXT, "templateID" TEXT NOT NULL, "summary" TEXT, "summaryText" TEXT NOT NULL DEFAULT '', "scratchpad" TEXT NOT NULL DEFAULT '', "llmUsage" TEXT, "createdAt" DATETIME NOT NULL, "updatedAt" DATETIME NOT NULL);

CREATE TABLE "meetingTask" ("id" TEXT PRIMARY KEY NOT NULL, "meetingID" TEXT NOT NULL REFERENCES "meeting"("id") ON DELETE CASCADE, "text" TEXT NOT NULL, "assigneePersonID" TEXT REFERENCES "person"("id") ON DELETE SET NULL, "assigneeName" TEXT, "priority" TEXT NOT NULL, "dueDate" DATETIME, "done" BOOLEAN NOT NULL DEFAULT 0);

CREATE INDEX "meetingTask_meetingID" ON "meetingTask"("meetingID");

CREATE VIRTUAL TABLE "meeting_ft" USING fts5(title, summaryText, tokenize='''unicode61''', content='meeting');

CREATE TABLE 'meeting_ft_config'(k PRIMARY KEY, v) WITHOUT ROWID;

CREATE TABLE 'meeting_ft_data'(id INTEGER PRIMARY KEY, block BLOB);

CREATE TABLE 'meeting_ft_docsize'(id INTEGER PRIMARY KEY, sz BLOB);

CREATE TABLE 'meeting_ft_idx'(segid, term, pgno, PRIMARY KEY(segid, term)) WITHOUT ROWID;

CREATE INDEX "meeting_startedAt" ON "meeting"("startedAt");

CREATE INDEX "meeting_state" ON "meeting"("state");

CREATE TABLE "pairedDevice" ("id" TEXT PRIMARY KEY NOT NULL, "name" TEXT NOT NULL, "pairedAt" DATETIME NOT NULL, "lastSeenAt" DATETIME, "tokenHash" BLOB NOT NULL UNIQUE);

CREATE TABLE "participant" ("id" TEXT PRIMARY KEY NOT NULL, "meetingID" TEXT NOT NULL REFERENCES "meeting"("id") ON DELETE CASCADE, "personID" TEXT REFERENCES "person"("id") ON DELETE SET NULL, "displayName" TEXT NOT NULL, "role" TEXT NOT NULL, "email" TEXT);

CREATE INDEX "participant_meetingID" ON "participant"("meetingID");

CREATE INDEX "participant_personID" ON "participant"("personID");

CREATE TABLE "person" ("id" TEXT PRIMARY KEY NOT NULL, "displayName" TEXT NOT NULL, "email" TEXT, "embedding" BLOB, "sampleCount" INTEGER NOT NULL DEFAULT 0, "createdAt" DATETIME NOT NULL);

CREATE TABLE "setting" ("key" TEXT PRIMARY KEY NOT NULL, "value" TEXT NOT NULL);

CREATE TABLE "speaker" ("id" TEXT PRIMARY KEY NOT NULL, "meetingID" TEXT NOT NULL REFERENCES "meeting"("id") ON DELETE CASCADE, "clusterLabel" TEXT NOT NULL, "assignment" TEXT NOT NULL, "personID" TEXT REFERENCES "person"("id") ON DELETE SET NULL, "similarity" DOUBLE, "embedding" BLOB, "sampleClipStart" DOUBLE, "sampleClipEnd" DOUBLE, "sampleClipURL" TEXT, "clusterConfidence" DOUBLE NOT NULL);

CREATE INDEX "speaker_meetingID" ON "speaker"("meetingID");

CREATE INDEX "speaker_personID" ON "speaker"("personID");

CREATE TABLE "transcriptSegment" ("id" TEXT PRIMARY KEY NOT NULL, "meetingID" TEXT NOT NULL REFERENCES "meeting"("id") ON DELETE CASCADE, "start" DOUBLE NOT NULL, "end" DOUBLE NOT NULL, "speakerID" TEXT REFERENCES "speaker"("id") ON DELETE SET NULL, "lane" TEXT NOT NULL, "text" TEXT NOT NULL, "rawText" TEXT NOT NULL);

CREATE VIRTUAL TABLE "transcriptSegment_ft" USING fts5(text, tokenize='''unicode61''', content='transcriptSegment');

CREATE TABLE 'transcriptSegment_ft_config'(k PRIMARY KEY, v) WITHOUT ROWID;

CREATE TABLE 'transcriptSegment_ft_data'(id INTEGER PRIMARY KEY, block BLOB);

CREATE TABLE 'transcriptSegment_ft_docsize'(id INTEGER PRIMARY KEY, sz BLOB);

CREATE TABLE 'transcriptSegment_ft_idx'(segid, term, pgno, PRIMARY KEY(segid, term)) WITHOUT ROWID;

CREATE INDEX "transcriptSegment_meetingID_start" ON "transcriptSegment"("meetingID", "start");

CREATE INDEX "transcriptSegment_speakerID" ON "transcriptSegment"("speakerID");
