# Handover admission ledger (schema v5)

P2 of `.plans/2026-10-07-stable-promotion.md` (D11): it lands before the
stable release, in both apps, with the Swift mirror in `Migrations.swift`.
It closes the three parts of the `fix/handover-lost-complete-answer` item in
"Open after the port" of `.plans/2026-10-02-rust-core-and-tauri-shell.md`
(lost answer, other bytes, older device) and the duplicate meeting that the
"Write order" parity note leaves to this change. Branch
`fix/handover-lost-complete-answer`.

## What is lost today

Nothing outlives a revoke. `handoverReceipt` holds the recording id, byte
count, SHA-256 and meeting id, but cascades from `pairedDevice`, and deleting
a meeting deletes its receipt. `meeting` and `audioAsset` hold no recording
id or hash. So a `complete` the computer admitted whose 200 never reached the
phone, followed by an unpair, makes the next pairing upload the recording
again as a second meeting. Every announce of other bytes under a known id,
and every announce of another device's id, is answered 409, so such a
recording never reaches the computer; it stays on the phone.

## The ledger

Migration v5 adds one table, with no foreign key and no change to an existing
table:

    CREATE TABLE "handoverAdmission" ("recordingID" TEXT NOT NULL,
      "byteCount" INTEGER NOT NULL, "sha256" BLOB NOT NULL,
      "meetingID" TEXT NOT NULL, "admittedAt" DATETIME NOT NULL,
      PRIMARY KEY ("recordingID", "byteCount", "sha256"))

One row per admitted file: the phone's recording id, its size and SHA-256,
the meeting it became and when. No audio, text or device name.

- **Written with the meeting.** The admission transaction of #213
  (`Store::save_admission_durably`, `MeetingStore.saveDurably(_:meeting:asset:)`)
  inserts the row (`INSERT OR IGNORE`) with the receipt, the meeting and the
  asset, so a row exists exactly when an admission committed. No other write
  site.
- **Backfilled on every open.** `Store::open` and `Store::in_memory` (not
  `open_without_migrating`, which writes nothing) and `MeetingStore.init` run
  `INSERT OR IGNORE INTO "handoverAdmission" SELECT ... FROM "handoverReceipt"
  WHERE "state" = 'complete'` for receipts whose meeting row exists, with the
  receipt's `updatedAt` as `admittedAt`. That covers the admissions before v5
  and those an older app commits during a rollback (it ignores v5 and writes
  no row).
- **Never deleted.** Neither a revoke nor a meeting delete touches it: an
  announce of a deleted meeting's recording is answered delivered (Nicolai,
  2026-10-07), so the phone deletes its copy instead of uploading it again.

## Two admissions of one phone recording id

`handoverReceipt` keeps `recordingID` as its primary key: a migration only
adds. A receipt is the working state of the current upload of a phone
recording id; the ledger is the history of what was admitted under it. A
re-announce with another size or SHA-256 replaces the receipt with a fresh
one for the new bytes and empty partial files; its `complete` admits a new
meeting with a fresh meeting id, and the ledger then holds both admissions,
told apart by size and SHA-256. The phone sees the same recording id
throughout and deletes its copy only on the 200 of a `complete`, which
answers only once the new meeting committed.

## Which receipt an announce lands on

The announce reads the ledger for the announced recording id, size and
SHA-256 first, then the receipt from memory or the store. Ledger rows are
never deleted, so a row read once stays true; a row that commits after the
read belongs to an admission whose receipt memory then holds as `complete`.
`E` is the receipt found, `A` the admission of the announced bytes.

| Receipt `E` | Admission `A` | Answer |
|---|---|---|
| none | yes | a `complete` receipt for the announcing device with `A`'s meeting id; files of the id discarded; 200 `complete`, every chunk listed |
| none | no | first announce, as today: 201 |
| same bytes, another device | either | takeover: the receipt becomes the announcing device's, files and chunks kept; then as below |
| same bytes, this device, `complete` | either | 200 `complete`, every chunk of the announced split (as today) |
| same bytes, this device, other chunk size | either | the partial restarts under the new split: files discarded and opened again, no chunk listed; 200 |
| same bytes, this device, same split | either | the re-announce as today (files reopened when missing): 200 |
| other bytes, `complete` or this device's | yes | a `complete` receipt with `A`'s meeting id, as for no receipt; 200 |
| other bytes, another device's, not `complete` | yes | 409 "another device owns this recording": that device's upload of other bytes is under way, and answering `complete` over it could delete that phone's copy |
| other bytes, any | no | a new recording: a fresh receipt for the announcing device, files discarded and opened; 201 |

The takeover needs no check that the new device replaced the old one: equal
size and SHA-256 prove the same bytes, so whichever device's `complete`
admits them, the other's copy is the same recording, and its next announce
finds a `complete` receipt of the same bytes and is answered delivered.

A stored receipt reads as admitted when the ledger holds its recording id,
size and SHA-256, whatever its state: it reads as `complete` with the
ledger's meeting id (`stored_receipt`, `HandoverEngine.storedReceipt`). That
also covers a `complete` receipt whose meeting the user deleted, and a line
save that put `verifying` or `receiving` back over the intake's `complete`
(the "Write order" note): after a restart the receipt reads `complete` and the
phone's retry gets the first meeting instead of a second admission. A
`complete` receipt with neither a meeting nor a ledger row still reads as
`failed` ("the admitted meeting is missing", #213).

Every receipt write of a request is dropped when memory holds another upload
by then: another device's receipt, as today, or one of other bytes
(`Engine::update`, `Engine::add_chunk`, `HandoverEngine.transition` and the
chunk fold), and a chunk is also dropped under another chunk size. Otherwise
a late `complete` of the replaced bytes would mark the new upload's receipt
`complete` with the old meeting, and the phone would delete a recording the
computer does not have.

The files of a replaced receipt go under the rules of the first announce
(`Engine::open_files`, the actor step in `RecordingHandler.announce`): the
replacement and its discard and opening are one step under the files lock in
Rust and one actor step in Swift, and a replacement declines when memory
holds another receipt by then and decides again with that one.

## What remains

- A takeover while the older device's `complete` is in the intake can still
  admit the bytes twice: when the intake commits before the takeover's save,
  that save writes the new device's unfinished receipt over the `complete`
  one, and the new device's upload admits a second meeting. A duplicate,
  never a lost recording; it needs two devices of the same recording live at
  once.
- A late request of a replaced upload (a refusal, a hash mismatch, the
  admission's cleanup) can still discard the new upload's files, as files
  are discarded by recording id and device. The phone then re-announces and
  sends its chunks again; nothing is lost.

## The migrator and the launch

- The Rust migrator ignores applied migrations it does not know, with a
  warning in the log, as GRDB does (`migrate`, `check`); the open that checks
  without migrating does too. `StoreError::UnknownMigration` goes. With the
  add-only rule (`.plans/2026-10-07-stable-promotion.md`, "Migrations add,
  never change") an older build keeps reading and writing a newer database.
- The desktop shell shows a dialog and logs the error when the store, or
  anything else the host needs, fails to open at launch, and exits, where it
  panicked before (`refuse_to_start` in `apps/desktop/src-tauri/src/main.rs`).

## Rollback

From `.tools`' research run on Forge and atlas, restated in the PR:

- Swift `v0.10.0-rc.2` (GRDB 7.11.1, `eraseDatabaseOnSchemaChange` false)
  opens a v5 database normally, ignores the v5 identifier, keeps writing, and
  writes no ledger row; the next open of a v5 build backfills those
  admissions.
- Rust `desktop-v0.1.0-rc.2` refuses a v5 database (`UnknownMigration`) and
  panics at launch; the file is untouched, and the install needs a newer
  build installed by hand. This build and later ones ignore later
  migrations.

## Tests

Both apps, each failing on the code before this change:

- A lost answer, then an unpair and a pairing again (same device id, and a
  new one), over a restarted engine: the announce answers 200 `complete`,
  `complete` answers the first meeting, the intake ran once.
- A deleted meeting's recording announced again: 200 `complete`, the deleted
  meeting's id, no admission.
- The backfill: a v4 database with an admitted receipt opened by this build;
  a receipt and meeting an older app committed after v5 (no ledger row),
  backfilled at the next open.
- Other bytes under a `complete` receipt: 201, a new admission and meeting,
  the phone's `complete` answered only after it; the ledger holds both.
- Another chunk size before `complete`: the partial restarts under the new
  split.
- The takeover: another device's announce of the same bytes takes over a
  receipt not yet `complete`, uploads the rest and completes.
- The upload guard: a late `complete` of replaced bytes leaves the new
  receipt unfinished.
- The migrator ignores a v6 applied to a copy, with a warning; the store
  still writes.
- The desktop shell refuses to start, with the dialog, on a store error.
