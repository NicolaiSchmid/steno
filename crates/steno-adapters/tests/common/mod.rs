//! The synthetic meeting every golden is rendered from, decoded from
//! `Tests/Fixtures/meetings/produktstrategie.json` (sequential UUIDs, fixed
//! dates, invented text), plus golden, temp-folder and store helpers.

#![allow(dead_code)]

use std::path::Path;

use chrono::{DateTime, TimeZone as _, Utc};
use chrono_tz::Tz;
use steno_adapters::rendering::{LinkStyle, RenderOptions};
use steno_core::{MeetingExport, Platform, Store};
use uuid::Uuid;

/// `00000000-0000-0000-0000-000000000001` for `n == 1`.
pub fn uuid(n: u32) -> Uuid {
    Uuid::from_u128(u128::from(n))
}

pub fn epoch(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0).unwrap()
}

pub fn meeting_id() -> Uuid {
    uuid(1)
}

pub const FOLDER_SLUG: &str = "2026-09-24-produktstrategie-90-10-roadmap-fuer-q4";
pub const FOLDER: &str = "Meetings/2026-09-24-produktstrategie-90-10-roadmap-fuer-q4";
pub const BERLIN: Tz = Tz::Europe__Berlin;

pub fn plain() -> RenderOptions {
    RenderOptions::PLAIN
}
pub fn plain_berlin() -> RenderOptions {
    RenderOptions {
        time_zone: BERLIN,
        ..RenderOptions::PLAIN
    }
}
/// The Obsidian destination's options with a people folder, in Berlin.
pub fn wikilink() -> RenderOptions {
    RenderOptions {
        link_style: LinkStyle::Wikilink,
        person_pages: true,
        time_zone: BERLIN,
        ..RenderOptions::PLAIN
    }
}
pub fn wikilink_tag() -> RenderOptions {
    RenderOptions {
        task_tag: Some("task".to_owned()),
        ..wikilink()
    }
}

/// The fixture meeting: three speakers (one unresolved), two persons,
/// fourteen segments, four tasks, two decisions, a mixdown.
pub fn export() -> MeetingExport {
    serde_json::from_slice(&fixture("meetings/produktstrategie.json")).unwrap()
}

/// The bytes of the test mixdown: 0 to 99.
pub fn mixdown_bytes() -> Vec<u8> {
    (0..100).collect()
}

/// The export with its mixdown at a fresh 100-byte file under `directory`.
pub fn export_with_audio_in(directory: &Path) -> MeetingExport {
    let mut export = export();
    let mixdown = directory.join("audio.m4a");
    std::fs::write(&mixdown, mixdown_bytes()).unwrap();
    export.audio.as_mut().unwrap().mixdown_url = Some(steno_core::paths::file_url(&mixdown, false));
    export
}

/// A file under `Tests/Fixtures/`.
pub fn fixture(relative: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../Tests/Fixtures")
        .join(relative);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

pub fn fixture_text(relative: &str) -> String {
    String::from_utf8(fixture(relative)).unwrap()
}

/// Asserts `actual` equals the golden at `Tests/Fixtures/<relative>`.
pub fn assert_matches_golden(actual: &str, relative: &str) {
    assert_eq!(actual, fixture_text(relative), "{relative} differs");
}

/// The folder note golden `variant` for a call on `platform`.
pub fn folder_note_golden(platform: Platform, variant: &str) -> String {
    match platform {
        Platform::Macos => format!("snapshots/obsidian/folder-note-{variant}.md"),
        Platform::Windows | Platform::Linux => {
            format!("snapshots/platforms/{platform}/folder-note-{variant}.md")
        }
    }
}

/// Writes the fixture meeting into `store` the way the pipeline does.
pub fn populate(store: &Store) -> MeetingExport {
    let export = export();
    for person in &export.persons {
        store.save_person(person).unwrap();
    }
    store
        .save_meeting_with_participants(&export.meeting, &export.participants)
        .unwrap();
    store.save_asset(export.audio.as_ref().unwrap()).unwrap();
    store
        .replace_transcript(&export.meeting, &export.segments, &export.speakers)
        .unwrap();
    let decisions: Vec<String> = export.decisions.iter().map(|d| d.text.clone()).collect();
    store
        .replace_summary(&export.meeting, &export.tasks, &decisions, &[])
        .unwrap();
    export
}

pub fn temp_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("steno-adapters-")
        .tempdir()
        .unwrap()
}

pub fn list(path: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}
