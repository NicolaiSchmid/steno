//! The recovery paths after the computer verified a file: the intake
//! failing once, the phone retrying by `complete` or by announcing again,
//! and a restart resuming from the stored receipt while the sweep removes
//! only what no receipt accounts for; after a restart and a revoke, another
//! phone's first announce of the same recording id starts from no file.

#![allow(
    clippy::assert_is_empty,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::large_futures,
    clippy::too_many_lines
)]

mod common;

use std::sync::Arc;

use common::{EngineDevice, Phone, ScriptedIntake, TestService, chunks, fake_intake, seeded_bytes};
use steno_core::testing::FakeHandoverIntake;
use steno_core::{
    AudioFormat, HandoverReceipt, HandoverState, HandoverStateKind, RecordingMetadata,
};
use steno_handover::engine::Engine;
use steno_handover::{HandoverIdentity, HandoverService, wire};
use uuid::Uuid;

const CHUNK_SIZE: i64 = 256 * 1024;

fn meeting_id() -> Uuid {
    Uuid::parse_str("1ABE1000-0000-4000-8000-0000000000AD").unwrap()
}

/// The computer comes back over `first`'s store and inbox with `intake`
/// (behind [`common::taking`], as the real intake takes the file), and the
/// phone resumes with the token it holds. The identity is new, so the
/// phone pins the new fingerprint.
async fn restart(
    first: &TestService,
    phone: &Phone,
    intake: Arc<FakeHandoverIntake>,
) -> (HandoverService, Phone) {
    let now = first.now;
    let second = HandoverService::new(
        first.service.configuration.clone(),
        first.store.clone(),
        common::taking(intake),
        Arc::new(HandoverIdentity::mint("Steno test identity", now).unwrap()),
        Arc::new(move || now),
    );
    second.start().await.unwrap();
    let steno_handover::ListenerState::Listening { port } = second.state() else {
        panic!("not listening");
    };
    let resumed = Phone {
        client: common::LoopbackClient::new(port, &second.identity.fingerprint()),
        token: phone.token.clone(),
        device_id: phone.device_id,
        device_name: phone.device_name.clone(),
    };
    (second, resumed)
}

#[tokio::test]
async fn intake_failure_is_500_and_the_next_complete_admits_the_same_verified_file() {
    let intake = ScriptedIntake::new(meeting_id(), 1);
    let test = TestService::with_intake(CHUNK_SIZE, intake.clone()).await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(2 * CHUNK_SIZE as usize, 77);
    let metadata = phone.metadata(&bytes, CHUNK_SIZE);
    phone.upload_all(&metadata, &bytes).await;
    let inbox = test.inbox();

    let failed = phone.complete(metadata.recording_id).await;
    assert_eq!(failed.status, 500);
    let problem = failed.json::<wire::Problem>().error;
    assert!(problem.contains("intake"));
    assert!(
        !problem.contains(inbox.directory.to_str().unwrap()),
        "the intake's error names the file; the phone must not learn the inbox path"
    );
    assert!(
        inbox.has_verified(metadata.recording_id, AudioFormat::M4aAac),
        "the verified file waits"
    );
    assert!(!inbox.has_partial(metadata.recording_id));
    assert_eq!(
        inbox.load_metadata(metadata.recording_id),
        Some(metadata.clone()),
        "the sidecar waits with it"
    );
    let receipt = test
        .store
        .handover_receipt(metadata.recording_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        receipt.state,
        HandoverState::Failed(Engine::INTAKE_REFUSED.to_owned()),
        "no path in the receipt either"
    );
    assert_eq!(
        receipt.received_chunks,
        vec![0, 1],
        "the chunk set survives the failure"
    );
    assert_eq!(
        phone
            .status(metadata.recording_id)
            .await
            .json::<wire::RecordingStatus>(),
        wire::RecordingStatus {
            state: HandoverStateKind::Failed,
            received_chunks: vec![0, 1]
        }
    );

    // The phone repeats the call; no chunk travels again.
    let retried = phone.complete(metadata.recording_id).await;
    assert_eq!(retried.status, 200);
    assert_eq!(
        retried.json::<wire::CompleteResponse>().meeting_id,
        meeting_id()
    );
    let files = intake.entries();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0], files[1], "the same verified file both times");
    assert_eq!(std::fs::read(&files[1]).unwrap(), bytes);
    assert_eq!(
        test.store
            .handover_receipt(metadata.recording_id)
            .unwrap()
            .unwrap()
            .state,
        HandoverState::Complete {
            meeting_id: meeting_id()
        }
    );
    assert!(
        inbox.load_metadata(metadata.recording_id).is_none(),
        "the sidecar goes with the success"
    );

    // And once more, for the phone that lost the 200: same id, no new admission.
    let again = phone.complete(metadata.recording_id).await;
    assert_eq!(
        again.json::<wire::CompleteResponse>().meeting_id,
        meeting_id()
    );
    assert_eq!(intake.count(), 2);
    test.stop().await;
}

#[tokio::test]
async fn re_announce_after_an_intake_failure_keeps_the_chunk_set() {
    // The phone's executor answers a 5xx at complete with a backoff and
    // then starts the recording over at announce.
    let intake = ScriptedIntake::new(meeting_id(), 1);
    let test = TestService::with_intake(CHUNK_SIZE, intake.clone()).await;
    let phone = Phone::pair(&test).await;
    let bytes = seeded_bytes(2 * CHUNK_SIZE as usize + 99, 78);
    let metadata = phone.metadata(&bytes, CHUNK_SIZE);
    let parts = chunks(&bytes, CHUNK_SIZE);
    phone.upload_all(&metadata, &bytes).await;
    assert_eq!(phone.complete(metadata.recording_id).await.status, 500);
    let inbox = test.inbox();

    let announced = phone.announce(&metadata).await;
    assert_eq!(announced.status, 200);
    assert_eq!(
        announced.json::<wire::RecordingStatus>(),
        wire::RecordingStatus {
            state: HandoverStateKind::Receiving,
            received_chunks: vec![0, 1, 2]
        },
        "every chunk is still there; the phone goes straight to complete"
    );
    assert!(
        !inbox.has_partial(metadata.recording_id),
        "no fresh partial beside the verified file"
    );
    assert!(inbox.has_verified(metadata.recording_id, AudioFormat::M4aAac));

    // A chunk the phone sends anyway is a harmless duplicate.
    assert_eq!(
        phone
            .upload(metadata.recording_id, 1, &parts[1])
            .await
            .status,
        204
    );
    let done = phone.complete(metadata.recording_id).await;
    assert_eq!(done.status, 200);
    assert_eq!(
        done.json::<wire::CompleteResponse>().meeting_id,
        meeting_id()
    );
    let files = intake.entries();
    assert_eq!(files.len(), 2);
    assert_eq!(std::fs::read(&files[1]).unwrap(), bytes);
    test.stop().await;
}

#[tokio::test]
async fn a_restarted_computer_resumes_from_the_stored_receipt_and_sweeps_only_orphans() {
    let first = TestService::with_chunk_size(CHUNK_SIZE).await;
    let phone = Phone::pair(&first).await;
    let bytes = seeded_bytes(3 * CHUNK_SIZE as usize, 79);
    let metadata = phone.metadata(&bytes, CHUNK_SIZE);
    let parts = chunks(&bytes, CHUNK_SIZE);
    assert_eq!(phone.announce(&metadata).await.status, 201);
    assert_eq!(
        phone
            .upload(metadata.recording_id, 0, &parts[0])
            .await
            .status,
        204
    );
    let inbox = first.inbox().clone();

    // Beside the live upload: an orphan nobody announced to the store, the
    // leftover of a completed handover, and a verified file whose intake
    // failed (its `failed` receipt keeps it for the retry).
    let seed = |id: Uuid| {
        let metadata = RecordingMetadata {
            recording_id: id,
            started_at: first.now,
            duration_seconds: 1.0,
            byte_count: 10,
            sha256: vec![0; 32],
            chunk_size: CHUNK_SIZE,
            format: AudioFormat::M4aAac,
            device_name: "Ghost".to_owned(),
        };
        inbox.begin(&metadata).unwrap();
    };
    let receipt = |id: Uuid, state: HandoverState| HandoverReceipt {
        recording_id: id,
        device_id: phone.device_id,
        state,
        byte_count: 10,
        sha256: vec![0; 32],
        chunk_size: CHUNK_SIZE,
        received_chunks: vec![0],
        created_at: first.now,
        updated_at: first.now,
    };
    let orphan = Uuid::new_v4();
    seed(orphan);
    let completed = Uuid::new_v4();
    seed(completed);
    inbox.promote(completed, AudioFormat::M4aAac).unwrap();
    let completed_meeting = Uuid::new_v4();
    common::save_admitted_meeting(&first.store, completed_meeting);
    first
        .store
        .save_handover_receipt(&receipt(
            completed,
            HandoverState::Complete {
                meeting_id: completed_meeting,
            },
        ))
        .unwrap();
    let refused = Uuid::new_v4();
    seed(refused);
    inbox.promote(refused, AudioFormat::M4aAac).unwrap();
    first
        .store
        .save_handover_receipt(&receipt(
            refused,
            HandoverState::Failed("admit: refused".to_owned()),
        ))
        .unwrap();
    first.stop().await;

    let intake = fake_intake(meeting_id());
    let (second, resumed) = restart(&first, &phone, intake.clone()).await;

    assert!(
        inbox.has_partial(metadata.recording_id),
        "the live upload is spared"
    );
    assert_eq!(
        inbox.load_metadata(metadata.recording_id),
        Some(metadata.clone())
    );
    assert!(
        inbox.has_verified(refused, AudioFormat::M4aAac),
        "the retryable verified file is spared"
    );
    assert!(
        !inbox.has_partial(orphan) && inbox.load_metadata(orphan).is_none(),
        "the orphan is gone"
    );
    assert!(
        !inbox.has_verified(completed, AudioFormat::M4aAac)
            && inbox.load_metadata(completed).is_none(),
        "the completed leftover is gone"
    );

    // The phone resumes with the token it holds; the receipt comes from the
    // store.
    let status = resumed.status(metadata.recording_id).await;
    assert_eq!(status.status, 200);
    assert_eq!(
        status.json::<wire::RecordingStatus>(),
        wire::RecordingStatus {
            state: HandoverStateKind::Receiving,
            received_chunks: vec![0]
        }
    );
    for (index, chunk) in parts.iter().enumerate().skip(1) {
        assert_eq!(
            resumed
                .upload(metadata.recording_id, index as i64, chunk)
                .await
                .status,
            204
        );
    }
    let completed_upload = resumed.complete(metadata.recording_id).await;
    assert_eq!(completed_upload.status, 200);
    assert_eq!(
        completed_upload.json::<wire::CompleteResponse>().meeting_id,
        meeting_id()
    );
    let admissions = intake.admissions.entries();
    assert_eq!(admissions.len(), 1);
    assert_eq!(std::fs::read(&admissions[0].file).unwrap(), bytes);
    second.stop().await;
}

#[tokio::test]
async fn another_phones_first_announce_after_a_restart_and_a_revoke_admits_its_own_bytes() {
    // The intake refuses the verified file once, and the computer restarts:
    // the receipt is only in the store, and the verified file waits for the
    // phone's retry. The phone is revoked, which finds no receipt in memory
    // to discard and deletes the row with the device. Another phone then
    // announces the same recording id: its first announce discards the
    // waiting file, so its `complete` hashes and admits its own bytes,
    // never the old file unhashed.
    const SIZE: i64 = 64 * 1024;
    let first = TestService::with(common::Options {
        chunk_size: SIZE,
        intake: Some(ScriptedIntake::new(meeting_id(), 1)),
        start: false,
        ..common::Options::default()
    })
    .await;
    let before = EngineDevice::paired(&first, "Old iPhone").await;
    let old_bytes = seeded_bytes(2 * SIZE as usize, 80);
    let old = before.metadata(&old_bytes, SIZE);
    let id = old.recording_id;
    before.upload_all(&old, &old_bytes).await;
    assert_eq!(before.complete(id).await.status.as_u16(), 500);
    let inbox = first.inbox();
    assert!(
        inbox.has_verified(id, old.format),
        "the verified file waits"
    );

    let intake = fake_intake(meeting_id());
    let second = Arc::new(HandoverService::new(
        first.service.configuration.clone(),
        first.store.clone(),
        common::taking(intake.clone()),
        first.service.identity.clone(),
        first.clock.clock(),
    ));
    second.engine.sweep_orphans().await;
    assert!(inbox.has_verified(id, old.format), "the sweep keeps it");
    second.revoke(before.device.id).await.unwrap();
    assert_eq!(first.store.handover_receipt(id).unwrap(), None);
    assert!(
        inbox.has_verified(id, old.format),
        "the revoke found no receipt in memory and left the file"
    );

    let other = EngineDevice::paired_on(&second, &first.store, "Other iPhone").await;
    let bytes = seeded_bytes(SIZE as usize + 99, 81);
    let metadata = common::metadata_for(&bytes, "Other iPhone", id, SIZE, old.format, None);
    let announced = other.announce(&metadata).await;
    assert_eq!(announced.status.as_u16(), 201);
    assert!(
        !inbox.has_verified(id, old.format),
        "the first announce discarded the waiting file"
    );
    assert_eq!(inbox.load_metadata(id), Some(metadata.clone()));
    other.upload_all(&metadata, &bytes).await;
    let completed = other.complete(id).await;
    assert_eq!(completed.status.as_u16(), 200);
    let admissions = intake.admissions.entries();
    assert_eq!(admissions.len(), 1);
    assert_eq!(
        std::fs::read(&admissions[0].file).unwrap(),
        bytes,
        "the other phone's own bytes, hashed"
    );
}

/// A `complete` receipt whose meeting never committed (earlier releases
/// committed the two separately, and a crash or a full disk could land in
/// between) is not admitted. The phone never got the 200 and holds the
/// recording, so after a restart the sweep keeps the verified file, a
/// re-announce lists every chunk without saying `complete`, and
/// `complete`, with or without that announce, admits the file again
/// instead of answering the missing meeting.
#[tokio::test]
async fn a_complete_receipt_without_its_meeting_is_admitted_again_after_a_restart() {
    let first = TestService::with_chunk_size(CHUNK_SIZE).await;
    let phone = Phone::pair(&first).await;
    let inbox = first.inbox().clone();
    let missing = Uuid::new_v4();
    let mut uploads = Vec::new();
    for seed in [80, 81] {
        let bytes = seeded_bytes(2 * CHUNK_SIZE as usize, seed);
        let metadata = phone.metadata(&bytes, CHUNK_SIZE);
        phone.upload_all(&metadata, &bytes).await;
        // Verified and handed to the intake, whose receipt committed and
        // whose meeting did not.
        inbox
            .promote(metadata.recording_id, metadata.format)
            .unwrap();
        let mut receipt = first
            .store
            .handover_receipt(metadata.recording_id)
            .unwrap()
            .unwrap();
        receipt.state = HandoverState::Complete {
            meeting_id: missing,
        };
        first.store.save_handover_receipt(&receipt).unwrap();
        uploads.push((metadata, bytes));
    }
    first.stop().await;

    let intake = fake_intake(meeting_id());
    let (second, resumed) = restart(&first, &phone, intake.clone()).await;

    for (metadata, _) in &uploads {
        assert!(
            inbox.has_verified(metadata.recording_id, metadata.format),
            "the sweep keeps the verified file"
        );
    }
    let announced = resumed.announce(&uploads[0].0).await;
    assert_eq!(announced.status, 200);
    assert_eq!(
        announced.json::<wire::RecordingStatus>(),
        wire::RecordingStatus {
            state: HandoverStateKind::Receiving,
            received_chunks: vec![0, 1]
        },
        "not complete: the phone goes on to complete"
    );
    for (metadata, _) in &uploads {
        let completed = resumed.complete(metadata.recording_id).await;
        assert_eq!(completed.status, 200);
        assert_eq!(
            completed.json::<wire::CompleteResponse>().meeting_id,
            meeting_id(),
            "the new admission's meeting, not the missing one"
        );
    }
    let admissions = intake.admissions.entries();
    assert_eq!(admissions.len(), 2, "both files are admitted again");
    for ((_, bytes), admission) in uploads.iter().zip(&admissions) {
        assert_eq!(&std::fs::read(&admission.file).unwrap(), bytes);
    }
    second.stop().await;
}
