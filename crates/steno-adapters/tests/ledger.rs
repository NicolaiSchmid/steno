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
        warnings: Vec::new(),
    }
}

#[test]
fn a_first_delivery_writes_everything() {
    let ledger = DeliveryLedger::new(None, ROOT, |_| false);
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
    let ledger = DeliveryLedger::new(Some(&previous), ROOT, |_| false);
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
    let ledger = DeliveryLedger::new(Some(&previous), ROOT, |_| false);
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
        let ledger = DeliveryLedger::new(Some(&with(folder, path)), ROOT, |_| false);
        assert!(ledger.is_first_delivery(), "{folder:?} {path:?}");
        assert_eq!(ledger.pinned_folder(), None, "{folder:?} {path:?}");
        assert!(ledger.files().is_empty(), "{folder:?} {path:?}");
        assert!(ledger.may_write(NOTE, true), "{folder:?} {path:?}");
    }
    let ledger = DeliveryLedger::new(Some(&with(LEDGER_FOLDER, NOTE)), ROOT, |_| false);
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
        DeliveryLedger::new(Some(&previous), "/vault", |_| false).pinned_folder(),
        Some(LEDGER_FOLDER)
    );
}

/// A receipt from a root the file system resolves to this one (a symlink,
/// another case on a case-insensitive disk) applies; the destination asks
/// its sink and passes the answer in.
#[test]
fn a_root_the_file_system_resolves_to_this_one_is_the_same_root() {
    let previous = receipt("/link-to-vault", &[(NOTE, FileOwnership::Owned)]);
    let applies = DeliveryLedger::new(Some(&previous), ROOT, |root| root == "/link-to-vault");
    assert_eq!(applies.pinned_folder(), Some(LEDGER_FOLDER));
    let other = DeliveryLedger::new(Some(&previous), ROOT, |_| false);
    assert!(other.is_first_delivery());
}

/// Copies beside a note are named by date, numbered from 2 on the same
/// day, and ordered by date and then number, not by name: `… 2)` comes
/// after `…)` and `… 10)` after `… 9)`.
#[test]
fn copies_beside_a_note_are_ordered_by_date_and_number() {
    let copy = |date: &str, number: u32| DeliveryLedger::copy_beside_path(NOTE, date, number);
    assert_eq!(
        copy("2026-10-07", 1),
        "Meetings/2026-09-24-sync/2026-09-24-sync (Steno 2026-10-07).md"
    );
    assert_eq!(
        copy("2026-10-07", 10),
        "Meetings/2026-09-24-sync/2026-09-24-sync (Steno 2026-10-07 10).md"
    );
    assert_eq!(
        DeliveryLedger::copy_beside_path("Meetings/x/meeting.json", "2026-10-07", 1),
        "Meetings/x/meeting (Steno 2026-10-07).json"
    );
    assert_eq!(
        DeliveryLedger::copy_stamp(NOTE, &copy("2026-10-07", 9)),
        Some(("2026-10-07", 9))
    );
    assert_eq!(
        DeliveryLedger::copy_stamp(NOTE, &copy("2026-10-07", 1)),
        Some(("2026-10-07", 1))
    );
    assert_eq!(DeliveryLedger::copy_stamp(NOTE, NOTE), None);
    assert_eq!(
        DeliveryLedger::copy_stamp(
            NOTE,
            "Meetings/2026-09-24-sync/2026-09-24-sync - Tasks (Steno 2026-10-07).md"
        ),
        None,
        "another note's copy"
    );

    let listed = |copies: &[String]| {
        let files: Vec<(&str, FileOwnership)> = copies
            .iter()
            .map(|path| (path.as_str(), FileOwnership::Owned))
            .collect();
        let previous = receipt(ROOT, &files);
        DeliveryLedger::new(Some(&previous), ROOT, |_| false)
            .listed_copy(NOTE)
            .map(str::to_owned)
    };
    assert_eq!(
        listed(&[copy("2026-10-07", 1), copy("2026-10-07", 2)]),
        Some(copy("2026-10-07", 2))
    );
    assert_eq!(
        listed(&[copy("2026-10-07", 9), copy("2026-10-07", 10)]),
        Some(copy("2026-10-07", 10))
    );
    assert_eq!(
        listed(&[copy("2026-10-07", 3), copy("2026-10-08", 1)]),
        Some(copy("2026-10-08", 1))
    );
    assert_eq!(listed(&[NOTE.to_owned()]), None);
}

/// A kept note keeps the hash Steno last wrote there, or gets this
/// render's when the receipt carries none.
#[test]
fn keep_records_the_delivered_hash_or_the_render_s() {
    let previous = receipt(ROOT, &[(NOTE, FileOwnership::Owned)]);
    let mut ledger = DeliveryLedger::new(Some(&previous), ROOT, |_| false);
    ledger.keep(NOTE, b"new render");
    let json = "Meetings/2026-09-24-sync/meeting.json";
    ledger.keep(json, b"new render");
    let kept = ledger.receipt(LEDGER_FOLDER, 1);
    let hash = |path: &str| {
        kept.files
            .iter()
            .find(|file| file.relative_path == path)
            .map(|file| (file.ownership, file.sha256.clone()))
    };
    assert_eq!(hash(NOTE), Some((FileOwnership::Owned, vec![1; 32])));
    assert_eq!(
        hash(json),
        Some((FileOwnership::Owned, sha256(b"new render")))
    );
}

#[test]
fn the_receipt_carries_unwritten_files_and_records_new_ones() {
    let previous = receipt(
        ROOT,
        &[(NOTE, FileOwnership::Owned), (AUDIO, FileOwnership::Owned)],
    );
    let mut ledger = DeliveryLedger::new(Some(&previous), ROOT, |_| false);
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
fn a_forgotten_file_leaves_the_receipt() {
    let previous = receipt(
        ROOT,
        &[(NOTE, FileOwnership::Owned), (AUDIO, FileOwnership::Owned)],
    );
    let mut ledger = DeliveryLedger::new(Some(&previous), ROOT, |_| false);
    ledger.forget(AUDIO);
    ledger.forget("Meetings/2026-09-24-sync/never-listed.md");
    let receipt = ledger.receipt(LEDGER_FOLDER, ArtifactRenderer::VERSION);
    assert_eq!(
        receipt
            .files
            .iter()
            .map(|f| f.relative_path.as_str())
            .collect::<Vec<_>>(),
        [NOTE],
        "a path never listed is no change"
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
fn moving_the_folder_drops_its_files_and_writes_the_claimed_one_as_on_a_first_delivery() {
    let previous = receipt(
        ROOT,
        &[
            (NOTE, FileOwnership::Owned),
            (ANNA, FileOwnership::ManagedBlock),
        ],
    );
    let mut ledger = DeliveryLedger::new(Some(&previous), ROOT, |_| false);
    let claimed = format!("{LEDGER_FOLDER}-2");
    ledger.move_folder(LEDGER_FOLDER, &claimed);

    assert_eq!(
        ledger.files().keys().collect::<Vec<_>>(),
        [ANNA],
        "only the lost folder's files leave"
    );
    assert!(ledger.may_write(&format!("{claimed}/meeting.json"), true));
    assert!(
        !ledger.may_write(&format!("{claimed}/sub/notes.md"), true),
        "directly in the claimed folder only"
    );
    assert!(
        !ledger.may_write(&format!("{claimed}0.md"), true),
        "a sibling with the same prefix is not in the claimed folder"
    );
    assert!(!ledger.may_write(AUDIO, true), "the lost folder: as before");
    assert_eq!(ledger.receipt(&claimed, 1).folder, claimed);
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
    let mut ledger = DeliveryLedger::new(Some(&previous), ROOT, |_| false);
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
    let mut ledger = DeliveryLedger::new(Some(&previous), ROOT, |_| false);
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
        DeliveryLedger::new(None, ROOT, |_| false).stale_managed_pages(&rendered(&[])),
        Vec::<String>::new()
    );
}
