//! The delivery ledger, which decides what a delivery may write so a file
//! the app never wrote is never opened, and the coordinator, which keeps
//! one row and receipt per destination so no export is lost or doubled.

mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use common::*;
use steno_adapters::obsidian::{ObsidianError, ObsidianFolderDestination};
use steno_adapters::runtime::{DeliveryCoordinator, DeliveryLedger, receipt_folder_path};
use steno_core::content_hash::sha256;
use steno_core::{
    BoundaryResult, DeliveredFile, Delivery, DeliveryDispatcher, DeliveryReceipt, DeliveryStatus,
    Destination, FileOwnership, MeetingExport, ObsidianSettings, Settings, Store, async_trait,
};
use uuid::Uuid;

const ROOT: &str = "/vault";
const LEDGER_FOLDER: &str = "Meetings/2026-09-24-sync";
const NOTE: &str = "Meetings/2026-09-24-sync/2026-09-24-sync.md";
const AUDIO: &str = "Meetings/2026-09-24-sync/audio.m4a";
const ANNA: &str = "People/Anna.md";
const BOB: &str = "People/Bob.md";

fn entry(path: &str, ownership: FileOwnership, data: &[u8]) -> DeliveredFile {
    DeliveredFile {
        relative_path: path.to_owned(),
        ownership,
        sha256: sha256(data),
    }
}

/// A receipt listing the note and the audio as owned, and Anna's and Bob's
/// pages as managed blocks, each with the hash of `b"steno"`.
fn receipt(root: &str) -> DeliveryReceipt {
    DeliveryReceipt {
        root: root.to_owned(),
        folder: LEDGER_FOLDER.to_owned(),
        files: vec![
            entry(AUDIO, FileOwnership::Owned, b"steno"),
            entry(NOTE, FileOwnership::Owned, b"steno"),
            entry(ANNA, FileOwnership::ManagedBlock, b"steno"),
            entry(BOB, FileOwnership::ManagedBlock, b"steno"),
        ],
        renderer_version: 1,
        warnings: Vec::new(),
    }
}

fn ledger(previous: &DeliveryReceipt) -> DeliveryLedger {
    DeliveryLedger::new(Some(previous), ROOT, |_| false)
}

#[test]
fn a_reexport_writes_only_what_the_receipt_lists_as_owned_or_what_is_absent() {
    let first = DeliveryLedger::new(None, ROOT, |_| false);
    assert!(first.is_first_delivery());
    assert!(
        first.may_write(NOTE, true),
        "a first delivery owns its folder"
    );

    let previous = receipt(ROOT);
    let mut again = ledger(&previous);
    assert_eq!(again.pinned_folder(), Some(LEDGER_FOLDER));
    let json = format!("{LEDGER_FOLDER}/meeting.json");
    for (path, exists, may) in [
        (NOTE, true, true),
        (json.as_str(), false, true),
        (json.as_str(), true, false),
        (ANNA, true, false),
    ] {
        assert_eq!(
            again.may_write(path, exists),
            may,
            "{path} exists: {exists}"
        );
    }
    again.record(&json, FileOwnership::Owned, b"json");
    assert!(
        !again.may_write(&json, true),
        "this run's records do not widen may_write"
    );

    let claimed = format!("{LEDGER_FOLDER}-2");
    again.move_folder(LEDGER_FOLDER, &claimed);
    assert_eq!(again.files().keys().collect::<Vec<_>>(), [ANNA, BOB]);
    assert!(again.may_write(&format!("{claimed}/meeting.json"), true));
    assert!(!again.may_write(&format!("{claimed}/sub/notes.md"), true));
    assert!(
        !again.may_write(&format!("{claimed}0.md"), true),
        "a sibling"
    );
    assert!(!again.may_write(&json, true), "the lost folder: as before");
}

/// A receipt applies to its own root under any spelling, or one the file
/// system resolves to it, and only while its paths stay inside.
#[test]
fn a_receipt_applies_only_to_its_own_root_and_paths_inside_it() {
    for (root, applies) in [
        ("/vault/", true),
        ("/vault/./Meetings/..", true),
        ("/../vault", true),
        ("/link-to-vault", true),
        ("/vault2", false),
        ("/elsewhere", false),
    ] {
        let previous = receipt(root);
        let ledger = DeliveryLedger::new(Some(&previous), ROOT, |root| root == "/link-to-vault");
        assert_eq!(!ledger.is_first_delivery(), applies, "{root}");
        assert_eq!(ledger.files().is_empty(), !applies, "{root}");
    }
    assert!(!DeliveryLedger::same_root("../vault", "vault"));
    for (folder, path) in [
        ("../elsewhere", NOTE),
        ("/Meetings/2026-09-24-sync", NOTE),
        ("./Meetings", NOTE),
        ("", NOTE),
        (LEDGER_FOLDER, "/etc/hosts"),
        (LEDGER_FOLDER, "Meetings/../../hosts"),
    ] {
        let mut previous = receipt(ROOT);
        previous.folder = folder.to_owned();
        previous.files[0].relative_path = path.to_owned();
        assert!(ledger(&previous).is_first_delivery(), "{folder:?} {path:?}");
        if folder != LEDGER_FOLDER {
            assert_eq!(
                receipt_folder_path(&previous),
                Path::new(ROOT),
                "{folder:?}"
            );
        }
    }
    assert_eq!(
        receipt_folder_path(&receipt("/vault/")),
        Path::new("/vault/").join(LEDGER_FOLDER)
    );
}

/// Copies beside an edited note are dated, numbered from 2, listed newest
/// first by date and number; bytes Steno wrote to the note or a copy are
/// Steno's.
#[test]
fn copies_beside_an_edited_note_and_whose_bytes_are_steno_s() {
    let copy = |date: &str, number: u32| DeliveryLedger::copy_beside_path(NOTE, date, number);
    assert_eq!(
        copy("2026-10-07", 1),
        "Meetings/2026-09-24-sync/2026-09-24-sync (Steno 2026-10-07).md"
    );
    assert_eq!(
        DeliveryLedger::copy_beside_path("x/meeting.json", "2026-10-07", 10),
        "x/meeting (Steno 2026-10-07 10).json"
    );
    let tasks_copy = "Meetings/2026-09-24-sync/2026-09-24-sync - Tasks (Steno 2026-10-07).md";
    assert_eq!(DeliveryLedger::copy_stamp(NOTE, tasks_copy), None);
    assert_eq!(
        DeliveryLedger::copy_stamp(NOTE, &copy("2026-10-07", 1).replace(')', " x)")),
        None
    );

    let mut previous = receipt(ROOT);
    for (date, number) in [("2026-10-07", 9), ("2026-10-08", 1), ("2026-10-07", 10)] {
        previous
            .files
            .push(entry(&copy(date, number), FileOwnership::Owned, b"copy"));
    }
    previous
        .files
        .push(entry(tasks_copy, FileOwnership::Owned, b"tasks copy"));
    let ledger = ledger(&previous);
    assert_eq!(
        ledger.listed_copies(NOTE),
        [
            copy("2026-10-08", 1),
            copy("2026-10-07", 10),
            copy("2026-10-07", 9)
        ]
    );
    assert!(ledger.wrote(NOTE, &sha256(b"steno")));
    assert!(ledger.wrote(NOTE, &sha256(b"copy")), "a copy beside it");
    assert!(
        !ledger.wrote(NOTE, &sha256(b"tasks copy")),
        "another note's copy"
    );
    assert!(!ledger.wrote(ANNA, &sha256(b"steno")), "a managed page");
    assert!(!ledger.wrote(NOTE, &sha256(b"the user's")));
}

/// `-2`, `-3`, … while a candidate is taken; one holding this meeting's
/// `meeting.json` is reused, and a failed claim ends the search.
#[test]
fn claiming_a_folder_suffixes_taken_ones_and_reuses_a_crashed_attempt() {
    let (ours, theirs) = (uuid(1), uuid(2));
    let base = LEDGER_FOLDER;
    let two = format!("{base}-2");
    let three = format!("{base}-3");
    let resolve = |taken: &[(&str, Option<Uuid>)]| {
        DeliveryLedger::claim_folder(
            base,
            ours,
            |candidate| Ok::<_, String>(!taken.iter().any(|(name, _)| *name == candidate)),
            |folder| {
                taken
                    .iter()
                    .find(|(name, _)| *name == folder)
                    .and_then(|(_, id)| *id)
            },
        )
        .unwrap()
    };
    assert_eq!(resolve(&[]), base);
    assert_eq!(resolve(&[(base, Some(theirs))]), two);
    assert_eq!(resolve(&[(base, None)]), two, "no meeting.json: taken too");
    assert_eq!(resolve(&[(base, Some(theirs)), (&two, None)]), three);
    assert_eq!(resolve(&[(base, Some(ours))]), base, "a crashed attempt");
    assert_eq!(resolve(&[(base, Some(theirs)), (&two, Some(ours))]), two);

    let failed = DeliveryLedger::claim_folder(
        base,
        ours,
        |candidate| {
            if candidate == base {
                Ok(false)
            } else {
                Err(format!("{candidate}: denied"))
            }
        },
        |_| Some(theirs),
    );
    assert_eq!(failed, Err(format!("{two}: denied")));
}

/// A destination that records the `previous` receipt it was handed.
struct RecordingDestination {
    id: String,
    failure: Option<ObsidianError>,
    previous: Mutex<Vec<Option<DeliveryReceipt>>>,
}

impl RecordingDestination {
    fn new(id: &str, failure: Option<ObsidianError>) -> Arc<Self> {
        Arc::new(RecordingDestination {
            id: id.to_owned(),
            failure,
            previous: Mutex::new(Vec::new()),
        })
    }

    fn handed(&self) -> Vec<Option<DeliveryReceipt>> {
        self.previous.lock().unwrap().clone()
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
        self.previous.lock().unwrap().push(previous.cloned());
        if let Some(failure) = &self.failure {
            return Err(Box::new(failure.clone()));
        }
        Ok(DeliveryReceipt {
            root: format!("/vault-{}", self.id),
            folder: format!("Meetings/{}", meeting.meeting.title),
            files: vec![],
            renderer_version: 7,
            warnings: Vec::new(),
        })
    }
}

fn now() -> DateTime<Utc> {
    export().meeting.updated_at
}

fn store() -> Arc<Store> {
    let store = Store::in_memory().unwrap();
    store.save_settings(&Settings::default()).unwrap();
    store.save_meeting(&export().meeting).unwrap();
    Arc::new(store)
}

fn coordinator(
    store: &Arc<Store>,
    destinations: &[&Arc<RecordingDestination>],
    clock: DateTime<Utc>,
) -> DeliveryCoordinator {
    let destinations: Vec<Arc<dyn Destination>> = destinations
        .iter()
        .map(|destination| Arc::clone(destination) as Arc<dyn Destination>)
        .collect();
    DeliveryCoordinator::with_destinations(
        Arc::clone(store),
        Box::new(move |_| destinations.clone()),
        Box::new(move || clock),
    )
}

/// One row per destination; a failure does not block the next, keeps the
/// stored receipt, and the next run is handed that receipt back.
#[tokio::test]
async fn every_destination_gets_a_row_and_its_stored_receipt_back() {
    let store = store();
    let missing = ObsidianError::VaultMissing("/vault".to_owned());
    let failing = RecordingDestination::new("a-fails", Some(missing.clone()));
    let working = RecordingDestination::new("b-works", None);
    let first = coordinator(&store, &[&failing, &working], now())
        .deliver_all(meeting_id())
        .await;
    assert_eq!(first[0].status, DeliveryStatus::Failed(missing.to_string()));
    assert_eq!(first[1].status, DeliveryStatus::Delivered);
    assert_eq!(first[1].id, Delivery::id_for(meeting_id(), "b-works"));
    assert!(first.iter().all(|d| d.last_attempt_at == Some(now())));
    assert_eq!(store.deliveries(meeting_id()).unwrap(), first);

    let broken = RecordingDestination::new("b-works", Some(missing));
    let later = now() + Duration::seconds(60);
    let second = coordinator(&store, &[&broken], later)
        .deliver_all(meeting_id())
        .await;
    assert!(matches!(second[0].status, DeliveryStatus::Failed(_)));
    assert_eq!(
        second[0].receipt, first[1].receipt,
        "the folder stays pinned"
    );
    assert_eq!(second[0].last_attempt_at, Some(later));

    let third = coordinator(&store, &[&working], later)
        .deliver_all(meeting_id())
        .await;
    assert_eq!(third[0].status, DeliveryStatus::Delivered);
    assert_eq!(working.handed(), [None, first[1].receipt.clone()]);
    assert_eq!(broken.handed(), [first[1].receipt.clone()]);
}

/// Unfinished rows of a destination no longer configured go, so the audio
/// expires; a delivered row keeps its receipt.
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

    assert_eq!(
        coordinator(&store, &[], now())
            .deliver_all(meeting_id())
            .await,
        Vec::<Delivery>::new()
    );
    assert_eq!(store.deliveries(meeting_id()).unwrap(), [delivered]);
    assert!(DeliveryCoordinator::destinations_for(&Settings::default()).is_empty());
}

/// Settings or an export that do not load fail the rows instead of
/// leaving silence, and keep every receipt.
#[tokio::test]
async fn a_failed_load_fails_the_rows_and_keeps_the_receipts() {
    let store = store();
    let working = RecordingDestination::new("b-works", None);
    let first = coordinator(&store, &[&working], now())
        .deliver_all(meeting_id())
        .await;

    let unknown = coordinator(&store, &[&working], now())
        .deliver_all(uuid(404))
        .await;
    let DeliveryStatus::Failed(reason) = &unknown[0].status else {
        panic!("{unknown:?}");
    };
    assert!(reason.starts_with("export failed: meeting "), "{reason}");

    store
        .write(|transaction| {
            transaction.execute(
                "INSERT OR REPLACE INTO setting (key, value) VALUES ('obsidian', '{not json')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    let results = coordinator(&store, &[&working], now())
        .deliver_all(meeting_id())
        .await;
    let DeliveryStatus::Failed(reason) = &results[0].status else {
        panic!("{results:?}");
    };
    assert!(reason.starts_with("settings failed: "), "{reason}");
    assert_eq!(results[0].receipt, first[0].receipt);
    assert_eq!(store.deliveries(meeting_id()).unwrap(), results);
    assert_eq!(
        working.handed().len(),
        1,
        "no destination ran after the first"
    );
}

/// Marking pending leaves a `Pending` row per configured destination that
/// keeps its receipt, so the launch finds the export owed; a row that does
/// not load is left as it is.
#[tokio::test]
async fn marking_an_export_pending_keeps_every_receipt() {
    let store = store();
    let first = RecordingDestination::new("a-first", None);
    let second = RecordingDestination::new("b-second", None);
    let delivered = coordinator(&store, &[&first], now())
        .deliver_all(meeting_id())
        .await;

    coordinator(&store, &[&first, &second], now()).mark_pending(meeting_id());

    let rows = store.deliveries(meeting_id()).unwrap();
    assert!(rows.iter().all(|row| row.status == DeliveryStatus::Pending));
    assert_eq!(
        rows.iter()
            .map(|row| row.receipt.clone())
            .collect::<Vec<_>>(),
        [delivered[0].receipt.clone(), None]
    );
    assert_eq!(
        store
            .meetings_with_unfinished_deliveries(now() + Duration::days(1), Duration::days(1))
            .unwrap(),
        [meeting_id()]
    );

    store
        .write(|transaction| {
            transaction.execute("UPDATE delivery SET receipt = '{not json'", [])?;
            Ok(())
        })
        .unwrap();
    coordinator(&store, &[&first], now()).mark_pending(meeting_id());
    assert!(store.deliveries(meeting_id()).is_err(), "not replaced");
}

/// The stored receipt survives the JSON column and makes the next run of
/// the real destination a byte-identical re-export.
#[tokio::test]
async fn the_stored_receipt_round_trips_into_the_obsidian_destination() {
    let store = store();
    let vault = temp_dir();
    let mut settings = store.settings().unwrap();
    settings.obsidian = Some(ObsidianSettings {
        vault_path: vault.path().to_string_lossy().into_owned(),
        people_folder: Some("People".to_owned()),
        include_audio: false,
        task_tag: None,
        extra: serde_json::Map::new(),
    });
    store.save_settings(&settings).unwrap();
    let ids: Vec<String> = DeliveryCoordinator::destinations_for(&settings)
        .iter()
        .map(|d| d.id().to_owned())
        .collect();
    assert_eq!(ids, [ObsidianFolderDestination::DESTINATION_ID]);
    let coordinator = DeliveryCoordinator::with_destinations(
        Arc::clone(&store),
        Box::new(|settings: &Settings| {
            let obsidian = settings.obsidian.clone().unwrap();
            vec![Arc::new(ObsidianFolderDestination::new(obsidian, BERLIN)) as Arc<dyn Destination>]
        }),
        Box::new(now),
    );

    let first = coordinator.deliver_all(meeting_id()).await;
    let receipt = first[0].receipt.clone().unwrap();
    assert_eq!(receipt.folder, FOLDER);
    assert_eq!(receipt.files.len(), 5, "no audio, no persons in the store");
    assert_eq!(
        store.deliveries(meeting_id()).unwrap()[0].receipt,
        Some(receipt.clone())
    );

    let notes = vault.path().join(FOLDER).join("notes.md");
    std::fs::write(&notes, b"mine\n").unwrap();
    let second = coordinator.deliver_all(meeting_id()).await;
    assert_eq!(second[0].receipt, Some(receipt));
    assert_eq!(std::fs::read(&notes).unwrap(), b"mine\n");
}
