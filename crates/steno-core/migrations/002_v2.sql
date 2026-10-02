-- GRDB migration "v2": the model's name guesses per speaker (#78).
CREATE TABLE "speakerNameSuggestion" ("speakerID" TEXT PRIMARY KEY NOT NULL REFERENCES "speaker"("id") ON DELETE CASCADE, "meetingID" TEXT NOT NULL REFERENCES "meeting"("id") ON DELETE CASCADE, "name" TEXT NOT NULL, "confidence" DOUBLE NOT NULL, "evidence" TEXT NOT NULL);
CREATE INDEX "speakerNameSuggestion_meetingID" ON "speakerNameSuggestion"("meetingID");
