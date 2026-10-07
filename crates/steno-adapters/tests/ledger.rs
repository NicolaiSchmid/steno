//! `DeliveryLedgerTests`: the delivery policy without a filesystem.

mod common;

use std::collections::BTreeMap;

use common::*;
use steno_adapters::rendering::ArtifactRenderer;
use steno_adapters::runtime::{DeliveryLedger, receipt_folder_path};
use steno_core::content_hash::sha256;
use steno_core::{DeliveredFile, DeliveryReceipt, FileOwnership};
use uuid::Uuid;

const ROOT: &str = "/vault";
const LEDGER_FOLDER: &str = "Meetings/2026-09-24-sync";
const NOTE: &str = "Meetings/2026-09-24-sync/2026-09-24-sync.md";
const AUDIO: &str = "Meetings/2026-09-24-sync/audio.m4a";
const ANNA: &str = "People/Anna.md";

fn receipt(root: &str, files: &[(&str, FileOwnership)]) -> DeliveryReceipt {
    let mut sorted: Vec<_> = files.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    DeliveryReceipt {
        root: root.to_owned(),
        folder: LEDGER_FOLDER.to_owned(),
        files: sorted
            .into_iter()
            .map(|(path, ownership)| DeliveredFile {
                relative_path: path.to_owned(),
                ownership,
                sha256: vec![1; 32],
            })
            .collect(),
        renderer_version: 1,
    }
}

#[test]
fn a_first_delivery_writes_everything() {
    let ledger = DeliveryLedger::new(None, ROOT);
    assert!(ledger.is_first_delivery());
    assert_eq!(ledger.pinned_folder(), None);
    assert!(
        ledger.may_write(NOTE, true),
        "a first delivery owns its folder"
    );
    assert!(ledger.may_write(NOTE, false));
    assert!(ledger.files().is_empty());
}

#[test]
fn may_write_refuses_a_file_the_app_never_wrote_on_reexport() {
    let previous = receipt(
        ROOT,
        &[
            (NOTE, FileOwnership::Owned),
            (ANNA, FileOwnership::ManagedBlock),
        ],
    );
    let ledger = DeliveryLedger::new(Some(&previous), ROOT);
    assert!(!ledger.is_first_delivery());
    assert_eq!(ledger.pinned_folder(), Some(LEDGER_FOLDER));
    assert!(ledger.may_write(NOTE, true), "listed as owned: rewritten");
    assert!(ledger.may_write(AUDIO, false), "absent: written");
    assert!(
        !ledger.may_write(AUDIO, true),
        "the user's file: never opened"
    );
    assert!(
        !ledger.may_write(ANNA, true),
        "a managed block is not owned outright"
    );
}

#[test]
fn a_receipt_from_another_root_is_a_first_delivery() {
    let previous = receipt("/elsewhere", &[(NOTE, FileOwnership::Owned)]);
    let ledger = DeliveryLedger::new(Some(&previous), ROOT);
    assert!(ledger.is_first_delivery());
    assert_eq!(ledger.pinned_folder(), None, "the other root pins nothing");
    assert!(ledger.files().is_empty(), "and carries nothing over");
    assert!(ledger.may_write(NOTE, true));
}

#[test]
fn a_receipt_whose_paths_leave_the_root_is_a_first_delivery() {
    let with = |folder: &str, path: &str| DeliveryReceipt {
        folder: folder.to_owned(),
        files: vec![DeliveredFile {
            relative_path: path.to_owned(),
            ownership: FileOwnership::Owned,
            sha256: vec![1; 32],
        }],
        ..receipt(ROOT, &[])
    };
    for (folder, path) in [
        ("../elsewhere", NOTE),
        ("/Meetings/2026-09-24-sync", NOTE),
        ("Meetings/../2026-09-24-sync", NOTE),
        ("./Meetings", NOTE),
        ("", NOTE),
        (LEDGER_FOLDER, "/etc/hosts"),
        (LEDGER_FOLDER, "Meetings/../../hosts"),
        (LEDGER_FOLDER, ""),
    ] {
        let ledger = DeliveryLedger::new(Some(&with(folder, path)), ROOT);
        assert!(ledger.is_first_delivery(), "{folder:?} {path:?}");
        assert_eq!(ledger.pinned_folder(), None, "{folder:?} {path:?}");
        assert!(ledger.files().is_empty(), "{folder:?} {path:?}");
        assert!(ledger.may_write(NOTE, true), "{folder:?} {path:?}");
    }
    let ledger = DeliveryLedger::new(Some(&with(LEDGER_FOLDER, NOTE)), ROOT);
    assert!(!ledger.is_first_delivery(), "plain relative paths apply");
    assert_eq!(ledger.pinned_folder(), Some(LEDGER_FOLDER));
}

#[test]
fn spellings_of_one_root_are_the_same_root() {
    assert!(DeliveryLedger::same_root("/vault", "/vault/"));
    assert!(DeliveryLedger::same_root("/vault", "/vault/./Meetings/.."));
    assert!(DeliveryLedger::same_root("/a/b", "/a//b"));
    assert!(!DeliveryLedger::same_root("/vault", "/vault2"));
    assert!(!DeliveryLedger::same_root("/vault", "/other/vault"));
    assert!(
        !DeliveryLedger::same_root("../vault", "vault"),
        "a leading `..` is not dropped"
    );
    assert!(DeliveryLedger::same_root("../vault", "../vault"));
    assert!(DeliveryLedger::same_root("a/../../vault", "../vault"));
    assert!(
        DeliveryLedger::same_root("/../vault", "/vault"),
        "nothing is above the root"
    );
    let previous = receipt("/vault/", &[(NOTE, FileOwnership::Owned)]);
    assert_eq!(
        DeliveryLedger::new(Some(&previous), "/vault").pinned_folder(),
        Some(LEDGER_FOLDER)
    );
}

#[test]
fn the_receipt_carries_unwritten_files_and_records_new_ones() {
    let previous = receipt(
        ROOT,
        &[(NOTE, FileOwnership::Owned), (AUDIO, FileOwnership::Owned)],
    );
    let mut ledger = DeliveryLedger::new(Some(&previous), ROOT);
    ledger.record(NOTE, FileOwnership::Owned, b"note\n");
    ledger.record(ANNA, FileOwnership::ManagedBlock, b"page\n");

    let receipt = ledger.receipt(LEDGER_FOLDER, ArtifactRenderer::VERSION);
    assert_eq!(receipt.root, ROOT);
    assert_eq!(receipt.folder, LEDGER_FOLDER);
    assert_eq!(receipt.renderer_version, ArtifactRenderer::VERSION);
    let mut expected = [NOTE, AUDIO, ANNA];
    expected.sort_unstable();
    assert_eq!(
        receipt
            .files
            .iter()
            .map(|f| f.relative_path.as_str())
            .collect::<Vec<_>>(),
        expected,
        "sorted by path; the opted-out audio stays listed"
    );
    let by_path: BTreeMap<&str, &DeliveredFile> = receipt
        .files
        .iter()
        .map(|f| (f.relative_path.as_str(), f))
        .collect();
    assert_eq!(by_path[NOTE].sha256, sha256(b"note\n"));
    let carried = previous
        .files
        .iter()
        .find(|f| f.relative_path == AUDIO)
        .unwrap();
    assert_eq!(by_path[AUDIO], carried, "carried over unchanged");
    assert_eq!(by_path[ANNA].ownership, FileOwnership::ManagedBlock);
    assert!(ledger.lists(LEDGER_FOLDER, |name| name.starts_with("audio.")));
    assert!(!ledger.lists("People", |name| name.starts_with("audio.")));
    assert!(
        !ledger.lists("Meetings", |name| name.starts_with("2026")),
        "only direct children count"
    );
}

#[test]
fn the_collision_rule_suffixes_taken_folders_and_reuses_a_crashed_attempt() {
    let ours = uuid(1);
    let theirs = uuid(2);
    let base = LEDGER_FOLDER;
    let resolve = |existing: &[(&str, Option<Uuid>)]| {
        DeliveryLedger::resolve_folder(
            base,
            ours,
            |folder| existing.iter().any(|(name, _)| *name == folder),
            |folder| {
                existing
                    .iter()
                    .find(|(name, _)| *name == folder)
                    .and_then(|(_, id)| *id)
            },
        )
    };
    let two = format!("{base}-2");
    assert_eq!(resolve(&[]), base);
    assert_eq!(
        resolve(&[(base, Some(theirs))]),
        two,
        "another meeting's folder"
    );
    assert_eq!(
        resolve(&[(base, None)]),
        two,
        "a folder without meeting.json is taken too"
    );
    assert_eq!(
        resolve(&[(base, Some(theirs)), (&two, None)]),
        format!("{base}-3")
    );
    assert_eq!(
        resolve(&[(base, Some(ours))]),
        base,
        "our own meeting.json: a crashed attempt, reused"
    );
    assert_eq!(resolve(&[(base, Some(theirs)), (&two, Some(ours))]), two);
}

#[test]
fn claiming_a_folder_tries_each_candidate_once_and_stops_at_a_failed_claim() {
    let ours = uuid(1);
    let base = LEDGER_FOLDER;
    let two = format!("{base}-2");
    let mut tried = Vec::new();
    let claimed = DeliveryLedger::claim_folder(
        base,
        ours,
        |candidate| {
            tried.push(candidate.to_owned());
            Ok::<_, String>(candidate == two)
        },
        |_| None,
    );
    assert_eq!(
        claimed,
        Ok(two.clone()),
        "the first candidate that is created"
    );
    assert_eq!(tried, [base.to_owned(), two.clone()]);

    let reused = DeliveryLedger::claim_folder(
        base,
        ours,
        |_| Ok::<_, String>(false),
        |folder| (folder == base).then_some(ours),
    );
    assert_eq!(reused, Ok(base.to_owned()), "a crashed attempt of ours");

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
        |_| Some(uuid(2)),
    );
    assert_eq!(failed, Err(format!("{two}: denied")));
}

#[test]
fn folder_path_joins_root_and_folder() {
    let receipt = receipt("/vault/", &[]);
    assert_eq!(
        receipt_folder_path(&receipt),
        std::path::Path::new("/vault/").join(LEDGER_FOLDER)
    );
}

#[test]
fn folder_path_falls_back_to_the_root_for_a_folder_that_leaves_it() {
    for folder in [
        "../elsewhere",
        "/Meetings/2026-09-24-sync",
        "./Meetings",
        "",
    ] {
        let receipt = DeliveryReceipt {
            folder: folder.to_owned(),
            ..receipt(ROOT, &[])
        };
        assert_eq!(
            receipt_folder_path(&receipt),
            std::path::Path::new(ROOT),
            "{folder:?}"
        );
    }
}

#[test]
fn this_runs_records_do_not_widen_may_write() {
    let previous = receipt(ROOT, &[(NOTE, FileOwnership::Owned)]);
    let mut ledger = DeliveryLedger::new(Some(&previous), ROOT);
    ledger.record(AUDIO, FileOwnership::Owned, b"audio");
    assert!(
        ledger.files().contains_key(AUDIO),
        "the receipt lists it for the next run"
    );
    assert!(
        !ledger.may_write(AUDIO, true),
        "the previous receipt decides; a file recorded this run is not owned yet"
    );
    assert!(ledger.may_write(AUDIO, false));
}

#[test]
fn stale_managed_pages_are_the_previous_blocks_not_rendered_this_time() {
    const BOB: &str = "People/Bob.md";
    let previous = receipt(
        ROOT,
        &[
            (NOTE, FileOwnership::Owned),
            (AUDIO, FileOwnership::Owned),
            (ANNA, FileOwnership::ManagedBlock),
            (BOB, FileOwnership::ManagedBlock),
        ],
    );
    let mut ledger = DeliveryLedger::new(Some(&previous), ROOT);
    let rendered = |paths: &[&str]| paths.iter().map(|p| (*p).to_owned()).collect();
    assert_eq!(ledger.stale_managed_pages(&rendered(&[ANNA])), [BOB]);
    assert_eq!(
        ledger.stale_managed_pages(&rendered(&[])),
        [ANNA, BOB],
        "owned files are never stale pages"
    );
    assert_eq!(
        ledger.stale_managed_pages(&rendered(&[ANNA, BOB])),
        Vec::<String>::new()
    );
    ledger.record(NOTE, FileOwnership::Owned, b"note");
    ledger.record("People/Cara.md", FileOwnership::ManagedBlock, b"page");
    assert_eq!(
        ledger.stale_managed_pages(&rendered(&[ANNA])),
        [BOB],
        "this run's records are not previous pages"
    );
    assert_eq!(
        DeliveryLedger::new(None, ROOT).stale_managed_pages(&rendered(&[])),
        Vec::<String>::new()
    );
}
