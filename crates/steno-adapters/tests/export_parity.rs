//! The export through `steno_core::Store` equals what the Swift CLI writes.
//!
//! `Tests/Fixtures/meetings/produktstrategie.json` is the `StenoJSON`
//! encoding of the fixture meeting as `steno export` writes it (the Swift
//! `MeetingJSONRendererTests` pins `StenoJSON.encode(export) == data`,
//! "byte-identical to steno export"). This test writes the same meeting into
//! a Rust store the way the pipeline does, reads it back through
//! `Store::export`, and renders `meeting.json` and every Markdown artifact
//! against the Swift goldens.
//!
//! One normalisation: both stores return decisions by id (GRDB's
//! `.order(DecisionRow.Columns.id)` and the Rust `ORDER BY id`), while the
//! fixture file lists them in extraction order, so the expected
//! `meeting.json` is the fixture with its two decisions in id order. The
//! printer's fidelity to Foundation is pinned separately by
//! `renderers::meeting_json_matches_the_golden_and_the_fixture_file`, which
//! compares the fixture bytes with no normalisation.

mod common;

use common::*;
use serde_json::Value;
use steno_adapters::rendering::{ArtifactRenderer, RenderedArtifactKind};
use steno_core::Store;
use steno_core::json::uuid_string;

/// The fixture `meeting.json` with `decisions` in id order, as both CLIs'
/// store export lists them.
fn fixture_in_store_order() -> String {
    let mut value: Value =
        serde_json::from_str(&fixture_text("meetings/produktstrategie.json")).unwrap();
    let decisions = value["decisions"].as_array_mut().unwrap();
    decisions.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));
    steno_bridge::json::to_canonical_string(&value).unwrap()
}

#[test]
fn a_meeting_written_through_the_store_exports_byte_identically_to_the_swift_cli() {
    let store = Store::in_memory().unwrap();
    let mut written = populate(&store);
    written
        .decisions
        .sort_by_key(|decision| uuid_string(decision.id));
    let export = store.export(meeting_id()).unwrap();
    assert_eq!(
        export, written,
        "the store returns what was written, in the Swift export's order"
    );

    let renderer = ArtifactRenderer::new();
    let json = renderer.render_json(&export).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&json),
        fixture_in_store_order(),
        "meeting.json"
    );

    let artifacts = renderer
        .render_meeting_files(&export, &wikilink(), None)
        .unwrap();
    let text = |kind: RenderedArtifactKind| {
        String::from_utf8(
            artifacts
                .iter()
                .find(|a| a.kind == kind)
                .unwrap()
                .data
                .clone(),
        )
        .unwrap()
    };
    // The folder note lists decisions in export order, so the store path
    // swaps the two lines of the golden's `## Decisions` section.
    let golden = fixture_text("snapshots/obsidian/folder-note-wikilink-berlin.md").replace(
        "- 90/10-Aufteilung wird umgesetzt.\n- Roadmap-Review am 15. Oktober.\n",
        "- Roadmap-Review am 15. Oktober.\n- 90/10-Aufteilung wird umgesetzt.\n",
    );
    assert_eq!(
        text(RenderedArtifactKind::FolderNote),
        golden,
        "folder note"
    );
    assert_matches_golden(
        &text(RenderedArtifactKind::Transcript),
        "snapshots/obsidian/transcript-wikilink.md",
    );
    assert_matches_golden(
        &text(RenderedArtifactKind::Tasks),
        "snapshots/obsidian/tasks-wikilink.md",
    );
    assert_matches_golden(
        &text(RenderedArtifactKind::Vtt),
        "snapshots/obsidian/transcript.vtt",
    );
}

#[test]
fn the_export_lists_persons_any_speaker_participant_or_task_points_at() {
    let store = Store::in_memory().unwrap();
    populate(&store);
    // A person nobody in the meeting points at is not exported.
    let stranger = steno_core::Person {
        id: uuid(99),
        display_name: "Aaron Stranger".to_owned(),
        email: None,
        embedding: None,
        sample_count: 0,
        created_at: created_at(),
    };
    store.save_person(&stranger).unwrap();
    let export = store.export(meeting_id()).unwrap();
    assert_eq!(
        export
            .persons
            .iter()
            .map(|p| p.display_name.as_str())
            .collect::<Vec<_>>(),
        ["Anna Müller", "Nicolai Schmid"]
    );
    assert!(matches!(
        store.export(uuid(404)),
        Err(steno_core::StoreError::MeetingNotFound(id)) if id == uuid(404)
    ));
}
