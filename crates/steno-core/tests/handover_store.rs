//! Paired devices and handover receipts: the rows the handover server
//! reads and writes, the cascade from a revoked device, and the three
//! column shapes of a receipt's state.

mod common;

use steno_core::*;

use common::{date, uuid};

const DEVICE_ID: &str = "0BADF00D-0000-4000-8000-000000000001";
const RECORDING_ID: &str = "6F9619FF-8B86-D011-B42D-00C04FC964FF";

fn device() -> PairedDevice {
    PairedDevice {
        id: uuid(DEVICE_ID),
        name: "Nicolai's iPhone".to_owned(),
        paired_at: date("2026-09-25T09:00:00.000Z"),
        last_seen_at: None,
    }
}

fn receipt(state: HandoverState) -> HandoverReceipt {
    HandoverReceipt {
        recording_id: uuid(RECORDING_ID),
        device_id: uuid(DEVICE_ID),
        state,
        byte_count: 3_000_000,
        sha256: vec![7; 32],
        chunk_size: 1024 * 1024,
        received_chunks: vec![0, 2],
        created_at: date("2026-09-25T09:01:00.000Z"),
        updated_at: date("2026-09-25T09:02:00.500Z"),
    }
}

#[test]
fn devices_are_found_by_token_hash_and_listed_oldest_first() {
    let store = Store::in_memory().unwrap();
    let hash = vec![1u8; 32];
    store.save_paired_device(&device(), &hash).unwrap();
    let later = PairedDevice {
        id: uuid("0BADF00D-0000-4000-8000-000000000002"),
        name: "Second".to_owned(),
        paired_at: date("2026-09-26T09:00:00.000Z"),
        last_seen_at: Some(date("2026-09-26T10:00:00.000Z")),
    };
    store.save_paired_device(&later, &[2u8; 32]).unwrap();

    assert_eq!(
        store.paired_device_for_token_hash(&hash).unwrap(),
        Some(device())
    );
    assert_eq!(
        store.paired_device_for_token_hash(&[9u8; 32]).unwrap(),
        None
    );
    assert_eq!(
        store.paired_device(uuid(DEVICE_ID)).unwrap(),
        Some(device())
    );
    assert_eq!(
        store.paired_devices().unwrap(),
        vec![device(), later.clone()]
    );

    // `save` replaces: a new token for the same device.
    let mut seen = device();
    seen.last_seen_at = Some(date("2026-09-27T09:00:00.000Z"));
    store.save_paired_device(&seen, &[3u8; 32]).unwrap();
    assert_eq!(store.paired_devices().unwrap().len(), 2);
    assert_eq!(store.paired_device_for_token_hash(&hash).unwrap(), None);
    assert_eq!(
        store.paired_device_for_token_hash(&[3u8; 32]).unwrap(),
        Some(seen)
    );
}

#[test]
fn touch_updates_the_live_row_and_resurrects_nothing() {
    let store = Store::in_memory().unwrap();
    let hash = vec![1u8; 32];
    store.save_paired_device(&device(), &hash).unwrap();
    let seen = date("2026-09-27T09:00:00.000Z");

    // A wrong token hash touches nothing.
    store
        .touch_paired_device(uuid(DEVICE_ID), &[9u8; 32], seen)
        .unwrap();
    assert_eq!(
        store.paired_device(uuid(DEVICE_ID)).unwrap(),
        Some(device())
    );

    // The right one moves `lastSeenAt` and nothing else.
    store
        .touch_paired_device(uuid(DEVICE_ID), &hash, seen)
        .unwrap();
    let mut expected = device();
    expected.last_seen_at = Some(seen);
    assert_eq!(
        store.paired_device_for_token_hash(&hash).unwrap(),
        Some(expected)
    );

    // The engine's shape: the device was read, a revoke landed, the touch
    // runs. The row stays gone and the token stays unknown.
    store.delete_paired_device(uuid(DEVICE_ID)).unwrap();
    store
        .touch_paired_device(uuid(DEVICE_ID), &hash, seen)
        .unwrap();
    assert_eq!(store.paired_device(uuid(DEVICE_ID)).unwrap(), None);
    assert_eq!(store.paired_device_for_token_hash(&hash).unwrap(), None);
    assert_eq!(store.paired_devices().unwrap(), Vec::new());
}

#[test]
fn receipts_round_trip_in_every_state_and_cascade_from_the_device() {
    let store = Store::in_memory().unwrap();
    store.save_paired_device(&device(), &[1u8; 32]).unwrap();
    assert_eq!(store.handover_receipt(uuid(RECORDING_ID)).unwrap(), None);

    for state in [
        HandoverState::Receiving,
        HandoverState::Verifying,
        HandoverState::Complete {
            meeting_id: uuid("516EADE8-40E5-4434-8AAF-9214A21A604E"),
        },
        HandoverState::Failed("sha256 mismatch".to_owned()),
    ] {
        let expected = receipt(state);
        store.save_handover_receipt(&expected).unwrap();
        assert_eq!(
            store.handover_receipt(uuid(RECORDING_ID)).unwrap(),
            Some(expected)
        );
    }

    let chunks: String = store
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT receivedChunks FROM handoverReceipt WHERE recordingID = ?1",
                [RECORDING_ID],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(
        chunks, "[0,2]",
        "the chunk set is one-line JSON, as GRDB writes it"
    );

    store.delete_paired_device(uuid(DEVICE_ID)).unwrap();
    assert_eq!(store.paired_devices().unwrap(), Vec::new());
    assert_eq!(
        store.handover_receipt(uuid(RECORDING_ID)).unwrap(),
        None,
        "the receipt goes with the device"
    );
}

#[test]
fn a_receipt_needs_its_device() {
    let store = Store::in_memory().unwrap();
    let error = store
        .save_handover_receipt(&receipt(HandoverState::Receiving))
        .unwrap_err();
    assert!(error.to_string().contains("FOREIGN KEY"), "{error}");
}

/// The ledger rows the admissions' transactions wrote, as SQLite holds
/// them: recording id, byte count, meeting id, admitted at.
fn ledger_rows(store: &Store) -> Vec<(String, i64, String, String)> {
    store
        .read(|connection| {
            let mut statement = connection.prepare(
                "SELECT recordingID, byteCount, meetingID, admittedAt FROM handoverAdmission \
                 ORDER BY admittedAt, meetingID",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .unwrap()
}

/// The meeting a receipt was admitted as, its asset, and the receipt
/// `complete` with it.
fn admission(meeting_id: &str, sha256: Vec<u8>) -> (HandoverReceipt, Meeting, AudioAsset) {
    let meeting = Meeting {
        id: uuid(meeting_id),
        ..common::meeting()
    };
    let asset = common::asset(meeting.id);
    let receipt = HandoverReceipt {
        sha256,
        ..receipt(HandoverState::Complete {
            meeting_id: meeting.id,
        })
    };
    (receipt, meeting, asset)
}

/// The admission's transaction writes the ledger row of the recording id,
/// size and SHA-256 with its meeting. A revoke's cascade takes the receipt
/// and a meeting delete takes the meeting and the receipt, and both leave
/// the row. Other bytes under the same recording id are a second
/// admission with a row of their own. The same bytes admitted again are
/// the meeting the ledger holds while it exists, and once it is deleted a
/// new meeting whose admission keeps the first row. Swift:
/// `anAdmissionWritesItsLedgerRow`.
#[test]
#[allow(clippy::too_many_lines)]
fn an_admission_writes_its_ledger_row_which_a_revoke_and_a_meeting_delete_leave() {
    let store = Store::in_memory().unwrap();
    store.save_paired_device(&device(), &[1; 32]).unwrap();
    let (first, meeting, asset) = admission("516EADE8-40E5-4434-8AAF-000000000001", vec![7; 32]);
    store
        .save_admission_durably(&first, &meeting, &asset)
        .unwrap();
    let recording_id = first.recording_id;
    let byte_count = first.byte_count;

    assert_eq!(
        store
            .admitted_meeting(recording_id, byte_count, &[7; 32])
            .unwrap(),
        Some(meeting.id)
    );
    assert_eq!(
        store
            .admitted_meeting(recording_id, byte_count + 1, &[7; 32])
            .unwrap(),
        None,
        "another size"
    );
    assert_eq!(
        store
            .admitted_meeting(recording_id, byte_count, &[8; 32])
            .unwrap(),
        None,
        "another SHA-256"
    );
    assert_eq!(
        ledger_rows(&store),
        [(
            RECORDING_ID.to_owned(),
            byte_count,
            "516EADE8-40E5-4434-8AAF-000000000001".to_owned(),
            "2026-09-25 09:02:00.500".to_owned()
        )],
        "the receipt's ids, size and time, as GRDB writes them"
    );

    // Over the stored receipt of other bytes the admission is refused and
    // writes nothing: built from that receipt, it would say those bytes
    // were admitted.
    let (other_bytes, second, second_asset) =
        admission("516EADE8-40E5-4434-8AAF-000000000002", vec![8; 32]);
    assert!(matches!(
        store.save_admission_durably(&other_bytes, &second, &second_asset),
        Err(StoreError::ReceiptOfAnotherUpload(id)) if id == recording_id
    ));
    assert_eq!(store.meeting(second.id).unwrap(), None);
    assert_eq!(ledger_rows(&store).len(), 1);
    // The announce of the other bytes saved their receipt first.
    let unfinished = |admitted: &HandoverReceipt| HandoverReceipt {
        state: HandoverState::Receiving,
        ..admitted.clone()
    };
    store
        .save_handover_receipt(&unfinished(&other_bytes))
        .unwrap();
    store
        .save_admission_durably(&other_bytes, &second, &second_asset)
        .unwrap();
    let (same_bytes, third, third_asset) =
        admission("516EADE8-40E5-4434-8AAF-000000000003", vec![7; 32]);
    store
        .save_handover_receipt(&unfinished(&same_bytes))
        .unwrap();
    assert_eq!(
        store
            .save_admission_durably(&same_bytes, &third, &third_asset)
            .unwrap(),
        meeting.id,
        "the same bytes are the meeting the ledger holds"
    );
    assert_eq!(store.meeting(third.id).unwrap(), None, "no second meeting");
    assert_eq!(
        store.handover_receipt(recording_id).unwrap().unwrap().state,
        HandoverState::Complete {
            meeting_id: meeting.id
        }
    );
    assert_eq!(
        store
            .admitted_meeting(recording_id, byte_count, &[8; 32])
            .unwrap(),
        Some(second.id),
        "other bytes are an admission of their own"
    );

    // Once the user deleted that meeting, the same bytes are admitted as a
    // new one, and the first admission's row stands.
    store.delete_meeting(meeting.id).unwrap();
    assert_eq!(store.handover_receipt(recording_id).unwrap(), None);
    store
        .save_handover_receipt(&unfinished(&same_bytes))
        .unwrap();
    assert_eq!(
        store
            .save_admission_durably(&same_bytes, &third, &third_asset)
            .unwrap(),
        third.id
    );
    store.delete_paired_device(device().id).unwrap();
    assert_eq!(
        store.handover_receipt(recording_id).unwrap(),
        None,
        "the revoke cascades"
    );
    assert_eq!(ledger_rows(&store).len(), 2, "the ledger keeps both rows");
    assert_eq!(
        store
            .admitted_meeting(recording_id, byte_count, &[7; 32])
            .unwrap(),
        Some(meeting.id)
    );
}

/// Every open backfills the ledger from the `complete` receipts whose
/// meeting row exists: a database an older build left at v4 (with the
/// rows the Swift `v0.10.0-rc.2` intake writes: a receipt and a meeting,
/// no ledger), and an admission an older app committed after v5 (it
/// ignores the table). A `complete` receipt whose meeting is missing and an
/// unfinished one get no row. Swift: `everyOpenBackfillsTheLedger`.
#[test]
fn every_open_backfills_the_ledger_from_admitted_receipts() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("steno.sqlite");
    let store = Store::open(&path).unwrap();
    store.save_paired_device(&device(), &[1; 32]).unwrap();
    let (admitted, meeting, asset) = admission("516EADE8-40E5-4434-8AAF-000000000001", vec![7; 32]);
    // The older intake's two commits, neither of which writes the ledger.
    store.save_meeting_with_asset(&meeting, &asset).unwrap();
    store.save_handover_receipt(&admitted).unwrap();
    let missing = HandoverReceipt {
        recording_id: uuid("6F9619FF-8B86-D011-B42D-000000000002"),
        ..receipt(HandoverState::Complete {
            meeting_id: uuid("516EADE8-40E5-4434-8AAF-0000000000FF"),
        })
    };
    store.save_handover_receipt(&missing).unwrap();
    let unfinished = HandoverReceipt {
        recording_id: uuid("6F9619FF-8B86-D011-B42D-000000000003"),
        ..receipt(HandoverState::Receiving)
    };
    store.save_handover_receipt(&unfinished).unwrap();
    // Back to v4: no table, no identifier, the rows the older build wrote.
    store
        .write(|transaction| {
            transaction.execute_batch(
                "DROP TABLE handoverAdmission;
                 DELETE FROM grdb_migrations WHERE identifier = 'v5';",
            )?;
            Ok(())
        })
        .unwrap();
    drop(store);
    let recorded: Vec<String> = rusqlite::Connection::open(&path)
        .unwrap()
        .prepare("SELECT identifier FROM grdb_migrations ORDER BY rowid")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(recorded, ["v1", "v2", "v3", "v4"]);

    let store = Store::open(&path).unwrap();
    assert_eq!(
        ledger_rows(&store),
        [(
            RECORDING_ID.to_owned(),
            admitted.byte_count,
            "516EADE8-40E5-4434-8AAF-000000000001".to_owned(),
            "2026-09-25 09:02:00.500".to_owned()
        )],
        "the admitted receipt only, admitted at its last update"
    );

    // An older app on the v5 database: a receipt and a meeting, no row.
    let (later, later_meeting, later_asset) =
        admission("516EADE8-40E5-4434-8AAF-000000000004", vec![9; 32]);
    let later = HandoverReceipt {
        recording_id: uuid("6F9619FF-8B86-D011-B42D-000000000004"),
        ..later
    };
    store
        .save_meeting_with_asset(&later_meeting, &later_asset)
        .unwrap();
    store.save_handover_receipt(&later).unwrap();
    assert_eq!(
        store
            .admitted_meeting(later.recording_id, later.byte_count, &[9; 32])
            .unwrap(),
        None
    );
    drop(store);
    let store = Store::open(&path).unwrap();
    assert_eq!(
        store
            .admitted_meeting(later.recording_id, later.byte_count, &[9; 32])
            .unwrap(),
        Some(later_meeting.id),
        "the next open backfills it"
    );
    assert_eq!(ledger_rows(&store).len(), 2);
}
