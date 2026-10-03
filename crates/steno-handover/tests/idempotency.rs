//! The receipt stream reaches `complete`, and a start sweeps orphaned
//! inbox files.

#![allow(
    clippy::assert_is_empty,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::large_futures,
    clippy::too_many_lines
)]

mod common;

use std::time::Duration;

use common::{Phone, TestService, date, fake_intake, seeded_bytes};
use steno_core::{
    AudioFormat, HandoverReceipt, HandoverState, HandoverStateKind, PairedDevice, RecordingMetadata,
};
use uuid::Uuid;

const CHUNK_SIZE: i64 = 1024 * 1024;

#[tokio::test]
async fn receipts_stream_reaches_complete() {
    let meeting_id = Uuid::parse_str("1DEA0000-0000-4000-8000-000000000002").unwrap();
    let test = TestService::with_intake(CHUNK_SIZE, fake_intake(meeting_id)).await;
    let mut receipts = test.service.receipts();
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(2 * CHUNK_SIZE as usize, 21);
    let metadata = phone.metadata(&bytes, CHUNK_SIZE);

    let collector = async {
        loop {
            let found = receipts
                .borrow_and_update()
                .iter()
                .find(|receipt| {
                    receipt.recording_id == metadata.recording_id
                        && receipt.state.kind() == HandoverStateKind::Complete
                })
                .cloned();
            if let Some(receipt) = found {
                return receipt;
            }
            receipts.changed().await.unwrap();
        }
    };
    let upload = async {
        phone.upload_all(&metadata, &bytes).await;
        assert_eq!(phone.complete(metadata.recording_id).await.status, 200);
    };
    let (receipt, ()) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(collector, upload)
    })
    .await
    .unwrap();
    assert_eq!(receipt.state, HandoverState::Complete { meeting_id });
    assert_eq!(receipt.received_chunks, vec![0, 1]);
    test.stop().await;
}

fn metadata(
    id: Uuid,
    device_name: &str,
    started_at: chrono::DateTime<chrono::Utc>,
) -> RecordingMetadata {
    RecordingMetadata {
        recording_id: id,
        started_at,
        duration_seconds: 10.0,
        byte_count: 1000,
        sha256: vec![0; 32],
        chunk_size: CHUNK_SIZE,
        format: AudioFormat::M4aAac,
        device_name: device_name.to_owned(),
    }
}

#[tokio::test]
async fn start_sweeps_orphaned_inbox_files() {
    let test = TestService::with(common::Options {
        chunk_size: CHUNK_SIZE,
        start: false,
        ..common::Options::default()
    })
    .await;
    // An announced-but-abandoned recording with no receipt in the store.
    let inbox = test.inbox();
    let orphan = Uuid::new_v4();
    inbox
        .begin(&metadata(orphan, "Ghost", date(1_789_000_000)))
        .unwrap();
    assert!(inbox.has_partial(orphan));

    test.service.start().await.unwrap();
    assert!(!inbox.has_partial(orphan), "the orphan partial is swept");
    assert!(inbox.load_metadata(orphan).is_none());
    test.stop().await;
}

#[tokio::test]
async fn start_sweeps_partials_abandoned_for_two_weeks() {
    // A phone that announced, uploaded most of a recording and was never
    // seen again would otherwise hold that space in the inbox for good.
    let test = TestService::with(common::Options {
        chunk_size: CHUNK_SIZE,
        start: false,
        ..common::Options::default()
    })
    .await;
    let inbox = test.inbox();
    let device = PairedDevice {
        id: Uuid::new_v4(),
        name: "Absent phone".to_owned(),
        paired_at: test.now,
        last_seen_at: None,
    };
    test.store.save_paired_device(&device, &[1u8; 32]).unwrap();
    let seed = |id: Uuid, age_seconds: i64, state: HandoverState| {
        let metadata = metadata(id, &device.name, test.now);
        inbox.begin(&metadata).unwrap();
        test.store
            .save_handover_receipt(&HandoverReceipt {
                recording_id: id,
                device_id: device.id,
                state,
                byte_count: 1000,
                sha256: metadata.sha256.clone(),
                chunk_size: CHUNK_SIZE,
                received_chunks: vec![0],
                created_at: test.now - chrono::Duration::seconds(age_seconds + 60),
                updated_at: test.now - chrono::Duration::seconds(age_seconds),
            })
            .unwrap();
    };
    let abandoned = Uuid::new_v4();
    seed(abandoned, 15 * 24 * 3600, HandoverState::Receiving);
    let stale = Uuid::new_v4();
    seed(
        stale,
        15 * 24 * 3600,
        HandoverState::Failed("sha256 mismatch".to_owned()),
    );
    let live = Uuid::new_v4();
    seed(live, 13 * 24 * 3600, HandoverState::Receiving);

    test.service.start().await.unwrap();
    assert!(!inbox.has_partial(abandoned) && inbox.load_metadata(abandoned).is_none());
    assert!(!inbox.has_partial(stale) && inbox.load_metadata(stale).is_none());
    assert!(inbox.has_partial(live), "thirteen days is not abandoned");
    assert!(
        test.store.handover_receipt(abandoned).unwrap().is_some(),
        "the receipt stays; a late re-announce starts over"
    );
    test.stop().await;
}
