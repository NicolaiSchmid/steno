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
