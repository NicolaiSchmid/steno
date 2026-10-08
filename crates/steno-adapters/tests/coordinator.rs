//! `DeliveryCoordinatorTests`.

mod common;

use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use common::*;
use steno_adapters::obsidian::{ObsidianError, ObsidianFolderDestination};
use steno_adapters::runtime::DeliveryCoordinator;
use steno_core::{
    BoundaryResult, DeliveredFile, Delivery, DeliveryDispatcher, DeliveryReceipt, DeliveryStatus,
    Destination, FileOwnership, MeetingExport, ObsidianSettings, Settings, Store, async_trait,
};

/// A destination that records the `previous` receipt it was handed.
struct RecordingDestination {
    id: String,
    failure: Option<ObsidianError>,
    previous_receipts: Mutex<Vec<Option<DeliveryReceipt>>>,
}

impl RecordingDestination {
    fn new(id: &str, failure: Option<ObsidianError>) -> Arc<Self> {
        Arc::new(RecordingDestination {
            id: id.to_owned(),
            failure,
            previous_receipts: Mutex::new(Vec::new()),
        })
    }

    fn entries(&self) -> Vec<Option<DeliveryReceipt>> {
        self.previous_receipts.lock().unwrap().clone()
    }
}

#[async_trait]
impl Destination for RecordingDestination {
    fn id(&self) -> &str {
        &self.id
    }

    async fn validate(&self) -> BoundaryResult<()> {
        Ok(())
    }

    async fn deliver(
        &self,
        meeting: &MeetingExport,
        previous: Option<&DeliveryReceipt>,
    ) -> BoundaryResult<DeliveryReceipt> {
        self.previous_receipts
            .lock()
            .unwrap()
            .push(previous.cloned());
        if let Some(failure) = &self.failure {
            return Err(Box::new(failure.clone()));
        }
        Ok(DeliveryReceipt {
            root: format!("/vault-{}", self.id),
            folder: format!("Meetings/{}", meeting.meeting.title),
            files: vec![DeliveredFile {
                relative_path: "meeting.json".to_owned(),
                ownership: FileOwnership::Owned,
                sha256: vec![if previous.is_none() { 1 } else { 2 }; 32],
            }],
            renderer_version: 7,
            warnings: Vec::new(),
        })
    }
}

fn now() -> DateTime<Utc> {
    updated_at()
}

fn store() -> Arc<Store> {
    let store = Store::in_memory().unwrap();
    store.save_settings(&Settings::default()).unwrap();
    let export = export();
    store.save_meeting(&export.meeting).unwrap();
    for person in &export.persons {
        store.save_person(person).unwrap();
    }
    Arc::new(store)
}

fn coordinator(
    store: &Arc<Store>,
    destinations: Vec<Arc<dyn Destination>>,
    clock: DateTime<Utc>,
) -> DeliveryCoordinator {
    DeliveryCoordinator::with_destinations(
        Arc::clone(store),
        Box::new(move |_| destinations.clone()),
        Box::new(move || clock),
    )
}

fn as_destination(destination: &Arc<RecordingDestination>) -> Arc<dyn Destination> {
    Arc::clone(destination) as Arc<dyn Destination>
}

#[tokio::test]
async fn one_row_per_destination_failures_do_not_block_the_next() {
    let store = store();
    let failing = RecordingDestination::new(
        "a-fails",
        Some(ObsidianError::VaultMissing("/vault".to_owned())),
    );
    let working = RecordingDestination::new("b-works", None);
    let coordinator = coordinator(
        &store,
        vec![as_destination(&failing), as_destination(&working)],
        now(),
    );

    let results = coordinator.deliver_all(meeting_id()).await;

    assert_eq!(
        results
            .iter()
            .map(|d| d.destination_id.as_str())
            .collect::<Vec<_>>(),
        ["a-fails", "b-works"]
    );
    assert_eq!(
        results[0].status,
        DeliveryStatus::Failed(ObsidianError::VaultMissing("/vault".to_owned()).to_string())
    );
    assert_eq!(results[0].receipt, None);
    assert_eq!(results[1].status, DeliveryStatus::Delivered);
    assert_eq!(results[1].receipt.as_ref().unwrap().root, "/vault-b-works");
    assert!(results.iter().all(|d| d.last_attempt_at == Some(now())));
    assert_eq!(
        results.iter().map(|d| d.id).collect::<Vec<_>>(),
        [
            Delivery::id_for(meeting_id(), "a-fails"),
            Delivery::id_for(meeting_id(), "b-works")
        ]
    );
    assert_eq!(
        store.deliveries(meeting_id()).unwrap(),
        results,
        "the rows are what deliver_all returned"
    );
    assert_eq!(failing.entries(), [None]);
    assert_eq!(working.entries(), [None]);
}

#[tokio::test]
async fn a_second_run_passes_the_stored_receipt_back_as_previous() {
    let store = store();
    let working = RecordingDestination::new("b-works", None);
    let first = coordinator(&store, vec![as_destination(&working)], now())
        .deliver_all(meeting_id())
        .await;
    let later = now() + Duration::seconds(60);
    let second = coordinator(&store, vec![as_destination(&working)], later)
        .deliver_all(meeting_id())
        .await;

    assert_eq!(working.entries(), [None, first[0].receipt.clone()]);
    assert_eq!(
        second[0].receipt.as_ref().unwrap().files[0].sha256,
        vec![2; 32]
    );
    assert_eq!(second[0].last_attempt_at, Some(later));
    assert_eq!(store.deliveries(meeting_id()).unwrap().len(), 1);
}

#[tokio::test]
async fn a_failed_run_keeps_the_previous_receipt_for_the_next_attempt() {
    let store = store();
    let working = RecordingDestination::new("x", None);
    let first = coordinator(&store, vec![as_destination(&working)], now())
        .deliver_all(meeting_id())
        .await;
    let broken =
        RecordingDestination::new("x", Some(ObsidianError::VaultMissing("/vault".to_owned())));
    let second = coordinator(&store, vec![as_destination(&broken)], now())
        .deliver_all(meeting_id())
        .await;
    assert!(matches!(second[0].status, DeliveryStatus::Failed(_)));
    assert_eq!(
        second[0].receipt, first[0].receipt,
        "the folder stays pinned across a failure"
    );
    let third = coordinator(&store, vec![as_destination(&working)], now())
        .deliver_all(meeting_id())
        .await;
    assert_eq!(working.entries(), [None, first[0].receipt.clone()]);
    assert_eq!(third[0].status, DeliveryStatus::Delivered);
}

#[tokio::test]
async fn no_obsidian_settings_means_no_destinations_and_no_rows() {
    let store = store();
    assert!(DeliveryCoordinator::destinations_for(&Settings::default()).is_empty());
    let coordinator = DeliveryCoordinator::with_destinations(
        Arc::clone(&store),
        Box::new(DeliveryCoordinator::destinations_for),
        Box::new(now),
    );
    assert_eq!(coordinator.deliver_all(meeting_id()).await, vec![]);
    assert_eq!(store.deliveries(meeting_id()).unwrap(), vec![]);

    let configured = Settings {
        obsidian: Some(ObsidianSettings {
            vault_path: "/tmp/vault".to_owned(),
            people_folder: Some("People".to_owned()),
            include_audio: false,
            task_tag: None,
            extra: serde_json::Map::new(),
        }),
        ..Settings::default()
    };
    let built = DeliveryCoordinator::destinations_for(&configured);
    assert_eq!(
        built.iter().map(|d| d.id().to_owned()).collect::<Vec<_>>(),
        [ObsidianFolderDestination::DESTINATION_ID]
    );
}

/// A row still outstanding for a destination that is no longer configured
/// would defer the audio's expiry forever and promise an export that never
/// runs; it is dropped. A delivered row keeps its receipt for a re-added
/// destination to update in place.
#[tokio::test]
async fn rows_of_a_removed_destination_are_dropped_unless_delivered() {
    let store = store();
    let failed = Delivery {
        id: Delivery::id_for(meeting_id(), "gone"),
        meeting_id: meeting_id(),
        destination_id: "gone".to_owned(),
        status: DeliveryStatus::Failed("vault missing".to_owned()),
        last_attempt_at: None,
        receipt: None,
    };
    let delivered = Delivery {
        id: Delivery::id_for(meeting_id(), "moved"),
        destination_id: "moved".to_owned(),
        status: DeliveryStatus::Delivered,
        ..failed.clone()
    };
    store.save_delivery(&failed).unwrap();
    store.save_delivery(&delivered).unwrap();

    let none = coordinator(&store, vec![], now());
    assert_eq!(none.deliver_all(meeting_id()).await, vec![]);
    let ids = |store: &Store| {
        store
            .deliveries(meeting_id())
            .unwrap()
            .iter()
            .map(|d| d.destination_id.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&store), ["moved"]);
    assert!(
        store
            .deliveries(meeting_id())
            .unwrap()
            .iter()
            .all(|d| d.status == DeliveryStatus::Delivered)
    );

    store.save_delivery(&failed).unwrap();
    let working = RecordingDestination::new("working", None);
    coordinator(&store, vec![as_destination(&working)], now())
        .deliver_all(meeting_id())
        .await;
    assert_eq!(ids(&store), ["moved", "working"]);
}

#[tokio::test]
async fn settings_that_do_not_load_fail_every_stored_row_instead_of_silence() {
    let store = store();
    let working = RecordingDestination::new("b-works", None);
    let first = coordinator(&store, vec![as_destination(&working)], now())
        .deliver_all(meeting_id())
        .await;
    assert_eq!(first[0].status, DeliveryStatus::Delivered);

    // A settings row that is not JSON makes `Store::settings` fail.
    store
        .write(|transaction| {
            transaction.execute(
                "INSERT OR REPLACE INTO setting (key, value) VALUES ('obsidian', '{not json')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(store.settings().is_err());

    let later = now() + Duration::seconds(60);
    let results = coordinator(&store, vec![as_destination(&working)], later)
        .deliver_all(meeting_id())
        .await;

    assert_eq!(
        results.len(),
        1,
        "one failed row per stored delivery, not silence"
    );
    let row = &results[0];
    assert_eq!(row.destination_id, "b-works");
    match &row.status {
        DeliveryStatus::Failed(reason) => {
            assert!(reason.starts_with("settings failed: "), "{reason}");
        }
        other => panic!("expected failed, got {other:?}"),
    }
    assert_eq!(row.last_attempt_at, Some(later));
    assert_eq!(
        row.receipt, first[0].receipt,
        "the receipt is kept for the next attempt"
    );
    assert_eq!(store.deliveries(meeting_id()).unwrap(), results);
    assert_eq!(working.entries().len(), 1, "no destination ran");
}

#[tokio::test]
async fn an_unknown_meeting_yields_failed_rows_not_a_panic() {
    let store = store();
    let working = RecordingDestination::new("b-works", None);
    let results = coordinator(&store, vec![as_destination(&working)], now())
        .deliver_all(uuid(404))
        .await;
    assert_eq!(results.len(), 1);
    match &results[0].status {
        DeliveryStatus::Failed(reason) => {
            assert!(reason.starts_with("export failed: meeting "), "{reason}");
        }
        other => panic!("expected failed, got {other:?}"),
    }
    assert_eq!(working.entries(), Vec::<Option<DeliveryReceipt>>::new());
    assert!(
        store.deliveries(uuid(404)).unwrap().is_empty(),
        "no meeting row, so no delivery row can hang off it; the result still reports the failure"
    );
}

#[tokio::test]
async fn the_stored_receipt_round_trips_into_the_real_destination() {
    let store = store();
    let directory = temp_dir("coordinator-vault");
    let mut configured = store.settings().unwrap();
    configured.obsidian = Some(ObsidianSettings {
        vault_path: directory.path().to_string_lossy().into_owned(),
        people_folder: Some("People".to_owned()),
        include_audio: false,
        task_tag: None,
        extra: serde_json::Map::new(),
    });
    store.save_settings(&configured).unwrap();
    let coordinator = DeliveryCoordinator::with_destinations(
        Arc::clone(&store),
        Box::new(|settings: &Settings| {
            settings
                .obsidian
                .as_ref()
                .map(|obsidian| {
                    vec![
                        Arc::new(ObsidianFolderDestination::new(obsidian.clone(), BERLIN))
                            as Arc<dyn Destination>,
                    ]
                })
                .unwrap_or_default()
        }),
        Box::new(now),
    );

    let first = coordinator.deliver_all(meeting_id()).await;
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].status, DeliveryStatus::Delivered);
    let receipt = first[0].receipt.clone().unwrap();
    assert_eq!(receipt.folder, FOLDER);
    assert_eq!(
        receipt.files.len(),
        5,
        "no audio, no persons in the store's export"
    );
    let stored = store.deliveries(meeting_id()).unwrap()[0]
        .receipt
        .clone()
        .unwrap();
    assert_eq!(stored, receipt, "the receipt survives the JSON column");

    // The user drops a note in; the second run through the stored receipt
    // changes nothing Steno wrote and leaves the note alone.
    let notes = directory.path().join(&receipt.folder).join("notes.md");
    std::fs::write(&notes, b"mine\n").unwrap();
    let second = coordinator.deliver_all(meeting_id()).await;
    assert_eq!(second[0].status, DeliveryStatus::Delivered);
    assert_eq!(
        second[0].receipt.as_ref(),
        Some(&receipt),
        "byte-identical files, identical hashes"
    );
    assert_eq!(std::fs::read(&notes).unwrap(), b"mine\n");
    assert_eq!(store.deliveries(meeting_id()).unwrap().len(), 1);
}

/// Marking the export pending writes a `Pending` row per configured
/// destination before the meeting is ready, keeping a stored receipt, so
/// the launch finds the export owed if the app ends before `deliver_all`;
/// with no destination configured it writes none.
#[tokio::test]
async fn marking_an_export_pending_leaves_a_row_per_destination_and_keeps_the_receipt() {
    let store = store();
    coordinator(&store, Vec::new(), now()).mark_pending(meeting_id());
    assert_eq!(store.deliveries(meeting_id()).unwrap(), []);
    let first = RecordingDestination::new("a-first", None);
    let second = RecordingDestination::new("b-second", None);
    let delivered = coordinator(&store, vec![as_destination(&first)], now())
        .deliver_all(meeting_id())
        .await;
    let both = coordinator(
        &store,
        vec![as_destination(&first), as_destination(&second)],
        now() + Duration::seconds(60),
    );

    both.mark_pending(meeting_id());

    let rows = store.deliveries(meeting_id()).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.status == DeliveryStatus::Pending));
    assert_eq!(rows[0].receipt, delivered[0].receipt);
    assert_eq!(rows[0].last_attempt_at, Some(now()));
    assert_eq!(rows[1].receipt, None);
    assert_eq!(rows[1].last_attempt_at, None);
    assert_eq!(
        store
            .meetings_with_unfinished_deliveries(now() + Duration::days(1), Duration::days(1))
            .unwrap(),
        [meeting_id()],
        "the launch finds the export owed"
    );
}

/// A stored row that does not read back (a receipt that is not JSON) is
/// left as it is: marking the export pending writes no row without the
/// receipt, which would make `deliver_all` write a new note.
#[tokio::test]
async fn marking_an_export_pending_writes_nothing_when_the_rows_do_not_load() {
    let store = store();
    let first = RecordingDestination::new("a-first", None);
    let marking = coordinator(&store, vec![as_destination(&first)], now());
    marking.deliver_all(meeting_id()).await;
    store
        .write(|transaction| {
            transaction.execute("UPDATE delivery SET receipt = '{not json'", [])?;
            Ok(())
        })
        .unwrap();
    assert!(store.deliveries(meeting_id()).is_err());

    marking.mark_pending(meeting_id());

    assert!(
        store.deliveries(meeting_id()).is_err(),
        "the unreadable row was replaced"
    );
}
