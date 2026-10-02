-- GRDB migration "v3": why a recording ended and where its title came from.
ALTER TABLE "meeting" ADD COLUMN "endReason" TEXT;
ALTER TABLE "meeting" ADD COLUMN "titleOrigin" TEXT NOT NULL DEFAULT 'default';
