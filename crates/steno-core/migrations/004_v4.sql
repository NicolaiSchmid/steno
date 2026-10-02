-- GRDB migration "v4": the learned per-stage rates behind progress estimates.
CREATE TABLE "stageRate" ("stage" TEXT NOT NULL, "key" TEXT NOT NULL, "samples" INTEGER NOT NULL, "secondsPerUnit" DOUBLE NOT NULL, "updatedAt" DATETIME NOT NULL, PRIMARY KEY ("stage", "key"));
