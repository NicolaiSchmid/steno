//! The export through `steno_core::Store` renders `meeting.json` and every
//! Markdown artifact as the goldens. The stores return decisions by id, the
//! fixture lists them in extraction order, so the expected `meeting.json`
//! has its two decisions in id order.

mod common;

use common::*;
use serde_json::Value;
use steno_adapters::rendering::{ArtifactRenderer, RenderedArtifactKind};
use steno_core::Store;
use steno_core::json::uuid_string;

/// The fixture `meeting.json` with `decisions` in id order.
fn fixture_in_store_order() -> String {
    let mut value: Value =
        serde_json::from_str(&fixture_text("meetings/produktstrategie.json")).unwrap();
    let decisions = value["decisions"].as_array_mut().unwrap();
    decisions.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));
    steno_core::json::to_canonical_string(&value).unwrap()
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
    // The store path swaps the golden's two decision lines.
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
