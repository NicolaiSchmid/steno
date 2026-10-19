//! A meeting written through `steno_core::Store` exports byte-identically
//! to the Swift CLI's `meeting.json` and renders the goldens. The store
//! returns decisions by id, the fixture in extraction order.

mod common;

use common::*;
use serde_json::Value;
use steno_adapters::rendering::{ArtifactRenderer, RenderedArtifactKind};
use steno_core::json::uuid_string;
use steno_core::{Person, Store};

#[test]
fn a_meeting_written_through_the_store_exports_byte_identically_to_the_swift_cli() {
    let store = Store::in_memory().unwrap();
    let mut written = populate(&store);
    // A person nobody in the meeting points at is not exported.
    store
        .save_person(&Person {
            id: uuid(99),
            display_name: "Aaron Stranger".to_owned(),
            ..written.persons[0].clone()
        })
        .unwrap();
    written
        .decisions
        .sort_by_key(|decision| uuid_string(decision.id));
    let export = store.export(meeting_id()).unwrap();
    assert_eq!(export, written);

    let mut swift: Value =
        serde_json::from_slice(&fixture("meetings/produktstrategie.json")).unwrap();
    swift["decisions"]
        .as_array_mut()
        .unwrap()
        .sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));
    let renderer = ArtifactRenderer::new();
    assert_eq!(
        String::from_utf8(renderer.render_json(&export).unwrap()).unwrap(),
        steno_core::json::to_canonical_string(&swift).unwrap(),
        "meeting.json"
    );

    let artifacts = renderer
        .render_meeting_files(&export, &wikilink(), None)
        .unwrap();
    let text = |kind| {
        let artifact = artifacts.iter().find(|a| a.kind == kind).unwrap();
        String::from_utf8(artifact.data.clone()).unwrap()
    };
    let folder_note = fixture_text("snapshots/obsidian/folder-note-wikilink-berlin.md").replace(
        "- 90/10-Aufteilung wird umgesetzt.\n- Roadmap-Review am 15. Oktober.\n",
        "- Roadmap-Review am 15. Oktober.\n- 90/10-Aufteilung wird umgesetzt.\n",
    );
    assert_eq!(text(RenderedArtifactKind::FolderNote), folder_note);
    for (kind, golden) in [
        (RenderedArtifactKind::Transcript, "transcript-wikilink.md"),
        (RenderedArtifactKind::Tasks, "tasks-wikilink.md"),
        (RenderedArtifactKind::Vtt, "transcript.vtt"),
    ] {
        assert_matches_golden(&text(kind), &format!("snapshots/obsidian/{golden}"));
    }
}
