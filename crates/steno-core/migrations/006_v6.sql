-- GRDB migration "v6": the user's answers about voices in a meeting. A
-- `speaker.dismissed` flag for a voice that is not a person (music, noise),
-- kept apart from the three-case `assignment`; and one row per suggestion
-- the user answered No to, so that person is not offered for that voice
-- again, with the voice match's score when there was one (a labelled
-- negative for the floor), and `speaker.confirmedSimilarity`, the score a
-- Yes was given (a labelled positive). None touches voice memory.
-- Mirrors "v6" in Sources/StenoCore/Storage/Migrations.swift; applied by
-- both sides until cutover; never edit once shipped.
ALTER TABLE "speaker" ADD COLUMN "dismissed" BOOLEAN NOT NULL DEFAULT 0;
ALTER TABLE "speaker" ADD COLUMN "confirmedSimilarity" DOUBLE;
CREATE TABLE "speakerSuggestionRejection" ("speakerID" TEXT NOT NULL REFERENCES "speaker"("id") ON DELETE CASCADE, "personID" TEXT NOT NULL REFERENCES "person"("id") ON DELETE CASCADE, "meetingID" TEXT NOT NULL REFERENCES "meeting"("id") ON DELETE CASCADE, "similarity" DOUBLE, PRIMARY KEY ("speakerID", "personID"));
CREATE INDEX "speakerSuggestionRejection_meetingID" ON "speakerSuggestionRejection"("meetingID");
