-- GRDB migration "v3": why a recording ended and where its title came from.
-- Mirrors "v3" in Sources/StenoCore/Storage/Migrations.swift; applied by
-- both sides until cutover; never edit once shipped.
ALTER TABLE "meeting" ADD COLUMN "endReason" TEXT;
ALTER TABLE "meeting" ADD COLUMN "titleOrigin" TEXT NOT NULL DEFAULT 'default';
