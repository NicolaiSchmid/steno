-- GRDB migration "v7": undoable combining and paragraph moves. A voice
-- combined into another keeps its own `speaker` row, embedding, clip and
-- answers, with `mergedInto` naming the voice it now reads as; its segments
-- keep its id, so "Separate again" is clearing the column. And
-- `meeting.transcriptGeneration`, raised each time processing writes the
-- transcript, so a paragraph move made against an older one is refused.
-- Mirrors "v7" in Sources/StenoCore/Storage/Migrations.swift; applied by
-- both sides until cutover; never edit once shipped.
ALTER TABLE "speaker" ADD COLUMN "mergedInto" TEXT REFERENCES "speaker"("id") ON DELETE SET NULL;
CREATE INDEX "speaker_mergedInto" ON "speaker"("mergedInto");
ALTER TABLE "meeting" ADD COLUMN "transcriptGeneration" INTEGER NOT NULL DEFAULT 0;
