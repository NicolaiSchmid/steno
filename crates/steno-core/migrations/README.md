# Migrations

The schema versions of the Steno database as SQL, applied by the Rust store
(`src/store/migrator.rs`) and recorded in `grdb_migrations` exactly as the
Swift app's GRDB migrator (`Sources/StenoCore/Storage/Migrations.swift`)
records its own. Both sides open the same file until cutover, so both must
know every version.

## Rules

- **Identifier.** `00N_vN.sql` holds the version GRDB knows as `vN`. The
  file number and the identifier never diverge.
- **Append-only.** A schema change is a new file and a new entry below the
  last one, never an edit to a file that has shipped. The Swift
  `SchemaSnapshotTests` and the Rust `schema_parity` test both fail on an
  edited version.
- **Both sides in one PR.** A new version ships as one PR, and neither side
  ships alone. GRDB ignores identifiers it does not know; the Rust store
  refuses them (`StoreError::UnknownMigration`). A Rust build shipping
  first would therefore leave the Swift app silently on a newer schema,
  while a Swift build shipping first only locks the Rust app out until it
  catches up.

## Adding a version

1. Add `migrations/00N_vN.sql` with the DDL and data moves, the same
   statements the Swift step runs. Plain SQL only: no GRDB helpers, no
   Rust-side code between statements.
2. Add the `Migration { identifier: "vN", sql: include_str!(...) }` entry at
   the end of `MIGRATIONS` in `src/store/migrator.rs`.
3. Add the Swift `Step(identifier: "vN", migrate: vN)` at the end of
   `Migrations.steps` in `Sources/StenoCore/Storage/Migrations.swift`,
   running the same SQL, and update the Swift schema snapshot golden.
4. Regenerate `tests/fixtures/schema.swift.sql` on a Mac with
   `scripts/dump-swift-schema.sh` and run `cargo test -p steno-core`: the
   parity test compares the Rust-made schema with it byte for byte.
5. Run `scripts/verify-store-against-swift-db.sh <steno.sqlite>` against a
   database the Swift app wrote: it opens a copy with the Rust store and
   prints what it reads.

The migrator runs each version in its own `IMMEDIATE` transaction with
foreign keys off and checks `pragma_foreign_key_check` before the commit, as
GRDB does; a version that leaves violating rows is rolled back and the open
fails with `StoreError::ForeignKeyViolations`.
