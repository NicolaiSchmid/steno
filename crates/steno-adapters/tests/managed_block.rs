//! `ManagedBlockTests`.

mod common;

use common::*;
use steno_adapters::obsidian::ManagedBlock;
use steno_adapters::rendering::{ArtifactRenderer, RenderOptions};

/// Anna's page, the first person in export order.
fn anna() -> steno_adapters::rendering::PersonPage {
    ArtifactRenderer::new()
        .render_person_pages(&export(), &wikilink(), Some(FOLDER_SLUG))
        .remove(0)
}

fn line() -> String {
    anna().line
}

const OLDER: &str =
    "- 2026-08-01 [[2026-08-01-kickoff|Kickoff]] %%steno:00000000-0000-0000-0000-000000000009%%";
const NEWER: &str =
    "- 2026-10-02 [[2026-10-02-retro|Retro]] %%steno:00000000-0000-0000-0000-000000000008%%";

#[test]
fn person_line_and_new_page() {
    let anna = anna();
    assert_eq!(
        anna.line,
        format!(
            "- 2026-09-24 [[{FOLDER_SLUG}|Produktstrategie: \"90/10\" & Roadmap für Q4]] %%steno:00000000-0000-0000-0000-000000000001%%"
        )
    );
    assert_eq!(anna.file_name, "Anna Müller.md");
    assert_eq!(
        anna.page,
        format!(
            "---\nsteno_person_id: \"00000000-0000-0000-0000-00000000000a\"\nemail: \"anna@example.com\"\ntype: \"person\"\n---\n\n# Anna Müller\n\n<!-- steno:meetings:start -->\n{}\n<!-- steno:meetings:end -->\n",
            anna.line
        )
    );
    assert_matches_golden(&anna.page, "snapshots/obsidian/person-page-new.md");
}

#[test]
fn appends_the_block_when_markers_are_missing() {
    let line = line();
    let merged = ManagedBlock::merge(&line, meeting_id(), "# Anna\n\nSome notes.");
    assert_eq!(
        merged,
        format!(
            "# Anna\n\nSome notes.\n\n<!-- steno:meetings:start -->\n{line}\n<!-- steno:meetings:end -->\n"
        )
    );
    assert_eq!(
        ManagedBlock::merge(&line, meeting_id(), ""),
        ManagedBlock::block(std::slice::from_ref(&line))
    );
}

#[test]
fn replaces_the_line_for_the_same_meeting_and_keeps_bytes_outside() {
    let line = line();
    let stale =
        "- 2026-09-24 [[old-slug|Old title]] %%steno:00000000-0000-0000-0000-000000000001%%";
    let page = format!(
        "---\nsteno_person_id: \"00000000-0000-0000-0000-00000000000a\"\ntype: \"person\"\n---\n# Anna Müller\n\nHer notes above the block.\n<!-- steno:meetings:start -->\n{OLDER}\n{stale}\n<!-- steno:meetings:end -->\nHer notes below the block, with %%steno:00000000-0000-0000-0000-000000000001%% mentioned.\n"
    );
    let merged = ManagedBlock::merge(&line, meeting_id(), &page);
    assert_eq!(
        merged,
        format!(
            "---\nsteno_person_id: \"00000000-0000-0000-0000-00000000000a\"\ntype: \"person\"\n---\n# Anna Müller\n\nHer notes above the block.\n<!-- steno:meetings:start -->\n{line}\n{OLDER}\n<!-- steno:meetings:end -->\nHer notes below the block, with %%steno:00000000-0000-0000-0000-000000000001%% mentioned.\n"
        )
    );
    let again = ManagedBlock::merge(NEWER, uuid(8), &merged);
    assert!(
        again.contains(&format!("start -->\n{NEWER}\n{line}\n{OLDER}\n<!--")),
        "newest first"
    );
    assert_matches_golden(&again, "snapshots/obsidian/person-page-merged.md");
    assert_eq!(
        ManagedBlock::merge(NEWER, uuid(8), &again),
        again,
        "idempotent"
    );
}

#[test]
fn remove_drops_only_this_meetings_line_and_keeps_bytes_outside() {
    let line = line();
    let page = format!(
        "---\nsteno_person_id: \"00000000-0000-0000-0000-00000000000a\"\ntype: \"person\"\n---\n# Anna Müller\n\nHer notes above the block.\n<!-- steno:meetings:start -->\n{NEWER}\n{line}\n{OLDER}\n<!-- steno:meetings:end -->\nHer notes below the block, with %%steno:00000000-0000-0000-0000-000000000001%% mentioned.\n"
    );
    let removed = ManagedBlock::remove(meeting_id(), &page);
    assert_eq!(
        removed,
        format!(
            "---\nsteno_person_id: \"00000000-0000-0000-0000-00000000000a\"\ntype: \"person\"\n---\n# Anna Müller\n\nHer notes above the block.\n<!-- steno:meetings:start -->\n{NEWER}\n{OLDER}\n<!-- steno:meetings:end -->\nHer notes below the block, with %%steno:00000000-0000-0000-0000-000000000001%% mentioned.\n"
        )
    );
    assert_eq!(
        ManagedBlock::remove(meeting_id(), &removed),
        removed,
        "a second removal changes nothing"
    );
    assert_eq!(
        ManagedBlock::merge(&line, meeting_id(), &removed),
        page,
        "merge puts the line back where it was"
    );
}

#[test]
fn remove_leaves_a_page_without_a_block_or_without_the_line_unchanged() {
    let line = line();
    let no_block = "# Anna\n\nSome notes with %%steno:00000000-0000-0000-0000-000000000001%%.\n";
    assert_eq!(ManagedBlock::remove(meeting_id(), no_block), no_block);
    assert_eq!(ManagedBlock::remove(meeting_id(), ""), "");
    let other = format!(
        "# Anna\n\n{}",
        ManagedBlock::block(std::slice::from_ref(&line))
    );
    assert_eq!(ManagedBlock::remove(uuid(8), &other), other);
    let open_ended = format!("# Anna\n\n<!-- steno:meetings:start -->\n{line}\n");
    assert_eq!(
        ManagedBlock::remove(meeting_id(), &open_ended),
        open_ended,
        "a start marker without an end is not a block"
    );
}

#[test]
fn remove_leaves_an_empty_block_with_its_markers() {
    let line = line();
    let page = format!(
        "# Anna\n\n{}\nBelow.\n",
        ManagedBlock::block(std::slice::from_ref(&line))
    );
    let removed = ManagedBlock::remove(meeting_id(), &page);
    assert_eq!(
        removed,
        format!("# Anna\n\n{}\nBelow.\n", ManagedBlock::block(&[]))
    );
    assert_eq!(
        ManagedBlock::merge(&line, meeting_id(), &removed),
        page,
        "the empty block takes the line back"
    );
}

#[test]
fn sorts_by_date_then_text() {
    let lines = [
        "- 2026-01-01 b",
        "- 2026-03-01 a",
        "- 2026-01-01 a",
        "no date at all",
    ]
    .map(str::to_owned)
    .to_vec();
    assert_eq!(
        ManagedBlock::sorted_newest_first(lines),
        [
            "- 2026-03-01 a",
            "- 2026-01-01 a",
            "- 2026-01-01 b",
            "no date at all"
        ]
    );
}

#[test]
fn a_hostile_title_cannot_break_out_of_the_person_line() {
    let mut export = export();
    export.meeting.title = "Sync | Q4 ]] %%steno:evil%% [x]\nline two".to_owned();
    let renderer = ArtifactRenderer::new();
    let line = renderer.render_person_pages(&export, &wikilink(), Some(FOLDER_SLUG))[0]
        .line
        .clone();
    let alias = "Sync / Q4 )\u{200B}) %%steno:evil%% (x) line two";
    let marker = "%%steno:00000000-0000-0000-0000-000000000001%%";
    assert_eq!(
        line,
        format!("- 2026-09-24 [[{FOLDER_SLUG}|{alias}]] {marker}")
    );
    assert_eq!(line.matches("]]").count(), 1, "exactly one link close");
    let merged = ManagedBlock::merge(&line, meeting_id(), "");
    let replaced = ManagedBlock::merge("- 2026-09-24 new", meeting_id(), &merged);
    assert_eq!(
        replaced,
        ManagedBlock::block(&["- 2026-09-24 new".to_owned()])
    );

    let plain_pages = RenderOptions {
        person_pages: true,
        time_zone: BERLIN,
        ..RenderOptions::PLAIN
    };
    let plain = renderer.render_person_pages(&export, &plain_pages, Some(FOLDER_SLUG))[0]
        .line
        .clone();
    assert_eq!(
        plain,
        format!("- 2026-09-24 [{alias}](</Meetings/{FOLDER_SLUG}/{FOLDER_SLUG}.md>) {marker}"),
        "plain style uses a root-absolute Markdown link to the folder note"
    );
}
