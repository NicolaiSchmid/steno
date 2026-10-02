-- GRDB migration "v4": the learned per-stage rates behind progress estimates.
-- Mirrors "v4" in Sources/StenoCore/Storage/Migrations.swift; applied by
-- both sides until cutover; never edit once shipped.
CREATE TABLE "stageRate" ("stage" TEXT NOT NULL, "key" TEXT NOT NULL, "samples" INTEGER NOT NULL, "secondsPerUnit" DOUBLE NOT NULL, "updatedAt" DATETIME NOT NULL, PRIMARY KEY ("stage", "key"));
