//! The renderers against the goldens in `Tests/Fixtures/snapshots/`, the
//! names Steno gives folders, notes and person pages, and the managed block
//! that keeps a user's edits to a person page.

mod common;

use chrono_tz::Tz;
use common::*;
use steno_adapters::naming::{MeetingFolder, Slug};
use steno_adapters::obsidian::ManagedBlock;
use steno_adapters::rendering::{
    ArtifactRenderer, Frontmatter, FrontmatterValue, RenderOptions, RenderedArtifactKind,
    markdown_text,
};
use steno_core::{MeetingExport, MeetingSource, MeetingTask, Platform, TaskPriority};

fn renderer() -> ArtifactRenderer {
    ArtifactRenderer::new()
}

#[test]
fn every_artifact_matches_its_golden() {
    let export = export();
    let r = renderer();
    let wikilink_utc = RenderOptions {
        time_zone: Tz::UTC,
        ..wikilink()
    };
    for (rendered, golden) in [
        (
            r.render_folder_note(&export, &plain(), None),
            "folder-note-plain-utc.md",
        ),
        (
            r.render_folder_note(&export, &plain_berlin(), None),
            "folder-note-plain-berlin.md",
        ),
        (
            r.render_folder_note(&export, &wikilink(), None),
            "folder-note-wikilink-berlin.md",
        ),
        (
            r.render_folder_note(&export, &wikilink_utc, None),
            "folder-note-wikilink-utc.md",
        ),
        (
            r.render_transcript(&export, &plain()),
            "transcript-plain.md",
        ),
        (
            r.render_transcript(&export, &wikilink()),
            "transcript-wikilink.md",
        ),
        (r.render_tasks(&export, &plain()), "tasks-plain.md"),
        (r.render_tasks(&export, &wikilink()), "tasks-wikilink.md"),
        (
            r.render_tasks(&export, &wikilink_tag()),
            "tasks-wikilink-tag.md",
        ),
        (r.render_vtt(&export), "transcript.vtt"),
    ] {
        assert_matches_golden(&rendered, &format!("snapshots/obsidian/{golden}"));
    }
    let json = r.render_json(&export).unwrap();
    assert_eq!(json, fixture("snapshots/obsidian/meeting.json"));
    assert_eq!(
        json,
        fixture("meetings/produktstrategie.json"),
        "round-trips"
    );
    assert!(
        fixture_text("snapshots/obsidian/VERSION")
            .starts_with(&format!("{} ", ArtifactRenderer::VERSION)),
        "a changed render bumps the renderer version"
    );
}

/// A call names the platform it was recorded on; in-person and phone
/// meetings never do, and the `source` key stays the stored one.
#[test]
fn the_folder_note_names_the_platform_of_a_call() {
    let mut export = export();
    for &platform in Platform::ALL {
        for (options, variant) in [(plain(), "plain-utc"), (wikilink(), "wikilink-berlin")] {
            let note = renderer().render_folder_note(
                &export,
                &RenderOptions {
                    platform,
                    ..options
                },
                None,
            );
            assert_matches_golden(&note, &folder_note_golden(platform, variant));
        }
    }
    for (source, key, labels) in [
        (
            MeetingSource::MacCall,
            "mac-call",
            ["Mac call", "Windows call", "Linux call"],
        ),
        (
            MeetingSource::MacInPerson,
            "mac-in-person",
            ["In person"; 3],
        ),
        (MeetingSource::Phone, "phone", ["Phone"; 3]),
    ] {
        export.meeting.source = source;
        for (&platform, label) in Platform::ALL.iter().zip(labels) {
            let note = renderer().render_folder_note(
                &export,
                &RenderOptions {
                    platform,
                    ..plain()
                },
                None,
            );
            assert!(note.contains(&format!("\nsource: \"{key}\"\n")), "{note}");
            assert!(
                note.contains(&format!(" · {label} · [Transcript]")),
                "{note}"
            );
        }
    }
}

#[test]
fn empty_sections_are_omitted_and_durations_round_up_to_minutes() {
    let mut export = export();
    export.decisions = vec![];
    export.meeting.scratchpad = "  \n".to_owned();
    export.meeting.summary = None;
    export.meeting.language = None;
    export.meeting.tags = vec!["  ".to_owned(), "###".to_owned()];
    export.meeting.duration = 45.0 * 60.0 + 1.0;
    let note = renderer().render_folder_note(&export, &wikilink(), Some("2026-09-24-pinned"));
    assert!(!note.contains("## Decisions"));
    assert!(!note.contains("## Scratchpad"));
    assert!(!note.contains("language:"));
    assert!(note.contains("\n## Summary\n\nNo summary.\n"));
    assert!(note.contains("\ntags:\n  - \"meeting\"\nsource:"));
    assert!(note.contains("\nduration: 46\n"));
    assert!(note.contains("[[2026-09-24-pinned - Transcript|Transcript]]"));
    for (duration, info) in [
        (7200.0, "14:00–16:00 · 2 h · "),
        (0.0, "14:00–14:00 · 0 min · "),
        // Half a millisecond under the boundary is not rounded onto 15:30.
        (5399.9995, "14:00–15:29 · 1 h 30 min · "),
        (f64::NAN, "14:00–14:00 · "),
        (-5.0, "14:00–14:00 · "),
    ] {
        export.meeting.duration = duration;
        let note = renderer().render_folder_note(&export, &wikilink(), None);
        assert!(
            note.contains(&format!("\n2026-09-24 {info}")),
            "{duration}: {note}"
        );
    }
    export.segments = vec![];
    export.tasks = vec![];
    assert!(
        renderer()
            .render_transcript(&export, &plain())
            .ends_with("— Transcript\n\nNo transcript.\n")
    );
    assert!(renderer().render_tasks(&export, &plain()).ends_with(
        "— Tasks\n\nNo tasks.\n\nEdit tasks in Steno; this file is rewritten on re-export.\n"
    ));
}

/// User text never breaks out of a YAML scalar, a heading, a link or a
/// voice span in the VTT file.
#[test]
fn hostile_titles_tags_and_names_stay_inside_their_syntax() {
    let mut export = export();
    export.meeting.title = "- yes: #no \"quoted\" \\ end\nsecond line".to_owned();
    export.meeting.tags = [
        "- x",
        "yes",
        "a:b",
        "#with space",
        "C/D_e",
        "🎉",
        "kunde/acme ",
    ]
    .map(str::to_owned)
    .to_vec();
    let note = renderer().render_folder_note(&export, &wikilink(), None);
    assert!(note.starts_with("---\ntitle: \"- yes: #no \\\"quoted\\\" \\\\ end\\nsecond line\"\n"));
    assert!(note.contains(
        "\ntags:\n  - \"meeting\"\n  - \"x\"\n  - \"yes\"\n  - \"ab\"\n  - \"with-space\"\n  - \"C/D_e\"\n  - \"kunde/acme\"\nsource:"
    ));
    assert!(note.contains("\n# - yes: #no \"quoted\" \\ end second line\n"));

    export.meeting.title = "Sync | Q4 ]] %%steno:evil%% [x]\nline two".to_owned();
    let line = &renderer().render_person_pages(&export, &wikilink(), Some(FOLDER_SLUG))[0].line;
    assert_eq!(
        *line,
        format!(
            "- 2026-09-24 [[{FOLDER_SLUG}|Sync / Q4 )\u{200B}) %%steno:evil%% (x) line two]] %%steno:00000000-0000-0000-0000-000000000001%%"
        )
    );
    let replaced = ManagedBlock::merge(
        "- 2026-09-24 new",
        meeting_id(),
        &ManagedBlock::merge(line, meeting_id(), ""),
    );
    assert_eq!(
        replaced,
        ManagedBlock::block(&["- 2026-09-24 new".to_owned()])
    );

    export.persons[1].display_name = "Tom <Tommy> & Söhne > GmbH\nBerlin".to_owned();
    export.meeting.title = "Q4 --> Q1 & <Plan>".to_owned();
    let vtt = renderer().render_vtt(&export);
    assert!(
        vtt.contains("\nNOTE\nQ4  Q1 & <Plan>\n"),
        "the NOTE only loses -->"
    );
    assert!(vtt.contains(
        "\n00:00:00.000 --> 00:00:04.200\n<v Tom <Tommy  Söhne  GmbH Berlin>Guten Morgen zusammen"
    ));
}

/// A person's page is named after them without the characters a file name
/// or wikilink cannot hold, and every note links that name.
#[test]
fn person_page_file_names_are_the_wikilink_targets() {
    let mut export = export();
    export.persons[0].display_name = "Anna/Müller: <CEO> [Acme]".to_owned();
    export.participants[0].display_name = export.persons[0].display_name.clone();
    let artifacts = renderer()
        .render_meeting_files(&export, &wikilink(), None)
        .unwrap();
    assert_eq!(
        artifacts.iter().map(|a| a.kind).collect::<Vec<_>>(),
        [
            RenderedArtifactKind::Json,
            RenderedArtifactKind::FolderNote,
            RenderedArtifactKind::Transcript,
            RenderedArtifactKind::Tasks,
            RenderedArtifactKind::Vtt
        ]
    );
    let text = |kind| {
        let artifact = artifacts.iter().find(|a| a.kind == kind).unwrap();
        String::from_utf8(artifact.data.clone()).unwrap()
    };
    let pages = renderer().render_person_pages(&export, &wikilink(), None);
    assert_eq!(pages[0].file_name, "AnnaMüller CEO Acme.md");
    assert!(pages[0].page.contains("# Anna/Müller: <CEO> [Acme]\n"));
    assert!(text(RenderedArtifactKind::FolderNote).contains("  - \"[[AnnaMüller CEO Acme]]\"\n"));
    assert!(
        text(RenderedArtifactKind::Transcript)
            .contains("\n## [[AnnaMüller CEO Acme]] — 00:00:04\n")
    );
    assert!(text(RenderedArtifactKind::Tasks).contains(" [[AnnaMüller CEO Acme]] "));
    assert_eq!(
        renderer().render_person_pages(&export, &plain(), None),
        vec![]
    );
}

fn task(n: u32, text: &str, assignee: Option<&str>, priority: TaskPriority) -> MeetingTask {
    MeetingTask {
        id: uuid(n),
        meeting_id: meeting_id(),
        text: text.to_owned(),
        assignee_person_id: None,
        assignee_name: assignee.map(str::to_owned),
        priority,
        due_date: None,
        done: false,
    }
}

fn task_lines(note: &str) -> Vec<&str> {
    note.lines()
        .filter(|line| line.starts_with("- ["))
        .collect()
}

/// Obsidian Tasks' field order: assignee, tag, priority, due date in the
/// options' time zone; an assignee named by cluster label is the person.
#[test]
fn task_lines_carry_priority_due_date_and_the_resolved_assignee() {
    let mut export: MeetingExport = export();
    export.tasks = vec![
        task(80, "Nur low", None, TaskPriority::Low),
        task(81, "Nur high", None, TaskPriority::High),
        task(82, "Nachfassen", Some("Speaker 1"), TaskPriority::Normal),
        task(83, "Offen lassen", Some("Speaker 2"), TaskPriority::Normal),
        MeetingTask {
            // 2026-10-01T23:30:00Z: still the 1st in UTC, the 2nd in Berlin.
            due_date: Some(epoch(1_790_897_400)),
            done: true,
            ..task(84, "Spät", None, TaskPriority::Normal)
        },
    ];
    let options = RenderOptions {
        task_tag: Some("#steno tasks".to_owned()),
        ..wikilink()
    };
    assert_eq!(
        task_lines(&renderer().render_tasks(&export, &options)),
        [
            "- [ ] Nur low #steno-tasks \u{1F53D}",
            "- [ ] Nur high #steno-tasks \u{23EB}",
            "- [ ] Nachfassen [[Anna Müller]] #steno-tasks",
            "- [ ] Offen lassen Speaker 2 #steno-tasks",
            "- [x] Spät #steno-tasks \u{1F4C5} 2026-10-02",
        ]
    );
    assert!(
        task_lines(&renderer().render_tasks(&export, &plain()))
            .contains(&"- [x] Spät \u{1F4C5} 2026-10-01")
    );
}

#[test]
fn slugs_fold_to_ascii_and_cut_at_sixty_characters() {
    let twenty_words = (1..=20)
        .map(|n| format!("wort{n}"))
        .collect::<Vec<_>>()
        .join(" ");
    for (title, slug) in [
        (
            "Produktstrategie: \"90/10\" & Roadmap für Q4",
            "produktstrategie-90-10-roadmap-fuer-q4",
        ),
        ("Über Äpfel, Öl und Straße", "ueber-aepfel-oel-und-strasse"),
        (
            "Jérôme trifft Zoë in São Paulo",
            "jerome-trifft-zoe-in-sao-paulo",
        ),
        ("a\u{308}", "ae"),
        ("Straße İstanbul", "strasse-istanbul"),
        ("  --Hello,   World!--  ", "hello-world"),
        ("Weekly / Sync #3", "weekly-sync-3"),
        ("", "meeting"),
        ("🎉日本語", "meeting"),
        (
            &twenty_words,
            "wort1-wort2-wort3-wort4-wort5-wort6-wort7-wort8-wort9-wort10",
        ),
        (&"a".repeat(61), &"a".repeat(60)),
        (&format!("{}-b", "a".repeat(60)), &"a".repeat(60)),
    ] {
        assert_eq!(Slug::title(title), slug, "{title}");
    }
}

#[test]
fn file_names_drop_forbidden_characters_and_keep_off_windows_device_names() {
    for (name, file) in [
        ("Anna Müller", "Anna Müller"),
        ("a/b\\c:d*e?f\"g<h>i|j#k^l[m]n", "abcdefghijklmn"),
        ("  spaced   out \t name  ", "spaced out name"),
        ("bell\u{07}char\u{7F}", "bellchar"),
        ("...dots...", "dots"),
        ("...", "Unnamed"),
    ] {
        assert_eq!(Slug::file_name(name), file, "{name}");
    }
    for (name, on_windows) in [
        ("Con", "Con_"),
        ("nul.tar", "nul_.tar"),
        ("COM1", "COM1_"),
        ("lpt\u{B3}", "lpt\u{B3}_"),
        ("CONIN$", "CONIN$_"),
        ("  Con  ", "Con_"),
        ("COM0", "COM0"),
        ("Console", "Console"),
        ("x.con", "x.con"),
    ] {
        assert_eq!(Slug::file_name_reserving(name, true), on_windows, "{name}");
        assert_eq!(
            Slug::file_name_reserving(name, false),
            name.trim(),
            "{name}"
        );
    }
    let (file, link) = if cfg!(windows) {
        ("Con_", "[[Con_|Con]]")
    } else {
        ("Con", "[[Con]]")
    };
    assert_eq!(Slug::file_name("Con"), file);
    assert_eq!(markdown_text::wikilink("Con", None), link);
}

#[test]
fn the_meeting_folder_is_dated_in_the_destinations_time_zone() {
    let mut meeting = export().meeting;
    assert_eq!(MeetingFolder::path(&meeting, BERLIN), FOLDER);
    for (seconds, title, utc, berlin) in [
        // 2026-09-24T23:00:00Z, summer time.
        (1_790_290_800, "Late", "2026-09-24-late", "2026-09-25-late"),
        // 2026-12-01T23:00:00Z: CET is one hour ahead, not two.
        (
            1_796_166_000,
            "???",
            "2026-12-01-meeting",
            "2026-12-02-meeting",
        ),
    ] {
        meeting.started_at = epoch(seconds);
        title.clone_into(&mut meeting.title);
        assert_eq!(MeetingFolder::basename(&meeting, Tz::UTC), utc);
        assert_eq!(
            MeetingFolder::path(&meeting, BERLIN),
            format!("Meetings/{berlin}")
        );
    }
}

#[test]
fn frontmatter_encodes_every_value_kind_and_matches_the_golden() {
    let mut frontmatter = Frontmatter::new(BERLIN);
    let string = |text: &str| FrontmatterValue::String(text.to_owned());
    for (key, value) in [
        ("title", "Produktstrategie: \"90/10\" & Roadmap für Q4"),
        ("colon", "key: value"),
        ("backslash", "C:\\Users\\anna"),
        ("dash", "- not a list"),
        ("hash", "tag #1 here"),
        ("answer", "yes"),
        ("date_like", "2026-09-24"),
        ("empty", ""),
        ("control", "bell\u{07}tab\tnewline\ndel\u{7F}"),
    ] {
        frontmatter.append(key, string(value));
    }
    let started = export().meeting.started_at;
    frontmatter.append("date", FrontmatterValue::DateTime(started));
    frontmatter.append("day", FrontmatterValue::Date(started));
    frontmatter.append("duration", FrontmatterValue::Int(90));
    frontmatter.append("done", FrontmatterValue::Bool(false));
    frontmatter.append(
        "participants",
        FrontmatterValue::List(vec!["[[Anna Müller]]".to_owned(), "Speaker 2".to_owned()]),
    );
    frontmatter.append("nothing", FrontmatterValue::List(vec![]));
    let tags = [
        "meeting",
        "Kunde ACME",
        "#q4",
        "  ",
        "a/b",
        "-x-",
        "2026",
        "true",
        "null",
        "1e3",
    ];
    frontmatter.append(
        "tags",
        FrontmatterValue::List(
            tags.iter()
                .filter_map(|tag| markdown_text::tag(tag))
                .collect(),
        ),
    );
    assert_matches_golden(&frontmatter.encoded(), "snapshots/obsidian/frontmatter.md");
}

// The managed block on person pages: only this meeting's line changes;
// every other byte of the user's page stays.

/// Anna's page line for the fixture meeting.
fn line() -> String {
    renderer()
        .render_person_pages(&export(), &wikilink(), Some(FOLDER_SLUG))
        .remove(0)
        .line
}

const OLDER: &str =
    "- 2026-08-01 [[2026-08-01-kickoff|Kickoff]] %%steno:00000000-0000-0000-0000-000000000009%%";
const NEWER: &str =
    "- 2026-10-02 [[2026-10-02-retro|Retro]] %%steno:00000000-0000-0000-0000-000000000008%%";

#[test]
fn a_new_person_page_matches_the_golden_and_a_user_page_gets_the_block_appended() {
    let anna = renderer()
        .render_person_pages(&export(), &wikilink(), Some(FOLDER_SLUG))
        .remove(0);
    assert_eq!(anna.file_name, "Anna Müller.md");
    assert_matches_golden(&anna.page, "snapshots/obsidian/person-page-new.md");
    let line = line();
    assert_eq!(
        ManagedBlock::merge(&line, meeting_id(), "# Anna\n\nSome notes."),
        format!(
            "# Anna\n\nSome notes.\n\n{}",
            ManagedBlock::block(std::slice::from_ref(&line))
        )
    );
}

#[test]
fn merge_replaces_this_meetings_line_newest_first_and_keeps_bytes_outside() {
    let line = line();
    let stale =
        "- 2026-09-24 [[old-slug|Old title]] %%steno:00000000-0000-0000-0000-000000000001%%";
    let page = |lines: &str| {
        format!(
            "---\nsteno_person_id: \"00000000-0000-0000-0000-00000000000a\"\ntype: \"person\"\n---\n# Anna Müller\n\nHer notes above the block.\n<!-- steno:meetings:start -->\n{lines}\n<!-- steno:meetings:end -->\nHer notes below the block, with %%steno:00000000-0000-0000-0000-000000000001%% mentioned.\n"
        )
    };
    let merged = ManagedBlock::merge(&line, meeting_id(), &page(&format!("{OLDER}\n{stale}")));
    assert_eq!(merged, page(&format!("{line}\n{OLDER}")));
    let again = ManagedBlock::merge(NEWER, uuid(8), &merged);
    assert_matches_golden(&again, "snapshots/obsidian/person-page-merged.md");
    assert_eq!(
        ManagedBlock::merge(NEWER, uuid(8), &again),
        again,
        "idempotent"
    );

    let removed = ManagedBlock::remove(meeting_id(), &again);
    assert_eq!(removed, page(&format!("{NEWER}\n{OLDER}")));
    assert_eq!(ManagedBlock::remove(meeting_id(), &removed), removed);
    assert_eq!(
        ManagedBlock::merge(&line, meeting_id(), &removed),
        again,
        "back in place"
    );

    let only = format!(
        "# Anna\n\n{}Below.\n",
        ManagedBlock::block(std::slice::from_ref(&line))
    );
    let emptied = ManagedBlock::remove(meeting_id(), &only);
    assert_eq!(
        emptied,
        format!("# Anna\n\n{}Below.\n", ManagedBlock::block(&[]))
    );
    assert_eq!(ManagedBlock::merge(&line, meeting_id(), &emptied), only);
}

/// Lines the user typed between the markers keep their place and bytes,
/// CRLF included; the marked lines take the first marked line's place.
#[test]
fn user_lines_inside_the_block_keep_their_place_and_line_endings() {
    let line = line();
    let page = format!(
        "<!-- steno:meetings:start -->\nAbove, hers.\n{OLDER}\nBetween, hers.\n<!-- steno:meetings:end -->\n"
    );
    assert_eq!(
        ManagedBlock::merge(&line, meeting_id(), &page),
        format!(
            "<!-- steno:meetings:start -->\nAbove, hers.\n{line}\n{OLDER}\nBetween, hers.\n<!-- steno:meetings:end -->\n"
        )
    );
    let stale =
        "- 2026-09-24 [[old-slug|Old title]] %%steno:00000000-0000-0000-0000-000000000001%%";
    let crlf = format!(
        "# Anna\r\n\r\n<!-- steno:meetings:start -->\r\n{stale}\r\nHers.\r\n{OLDER}\r\n\r\n<!-- steno:meetings:end -->\r\nBelow.\r\n"
    );
    assert_eq!(
        ManagedBlock::merge(&line, meeting_id(), &crlf),
        format!(
            "# Anna\r\n\r\n<!-- steno:meetings:start -->\n{line}\n{OLDER}\r\nHers.\r\n<!-- steno:meetings:end -->\r\nBelow.\r\n"
        ),
        "her lines keep their CR; the blank line inside the block goes"
    );
    assert_eq!(
        ManagedBlock::remove(meeting_id(), &crlf),
        crlf.replace(&format!("{stale}\r\n"), "")
    );
}

/// Without a start marker followed by an end marker there is no block:
/// `merge` appends a fresh one, `remove` changes nothing, and no user line
/// is ever lost as the page settles.
#[test]
fn a_page_with_broken_markers_loses_no_line() {
    let line = line();
    let block = ManagedBlock::block(std::slice::from_ref(&line));
    for page in [
        format!("# Anna\n\n<!-- steno:meetings:start -->\n{OLDER}\n\n## Notes\n\nFirst.\n"),
        format!("<!-- steno:meetings:end -->\n{OLDER}\n<!-- steno:meetings:start -->\n"),
    ] {
        assert_eq!(ManagedBlock::remove(uuid(9), &page), page);
        let once = ManagedBlock::merge(&line, meeting_id(), &page);
        assert_eq!(once, format!("{page}\n{block}"));
        let settled = ManagedBlock::merge(&line, meeting_id(), &once);
        assert_eq!(ManagedBlock::merge(&line, meeting_id(), &settled), settled);
        assert!(settled.contains(&line));
        for kept in [OLDER, "## Notes", "First."] {
            assert_eq!(
                page.contains(kept),
                settled.contains(kept),
                "{kept}: {settled}"
            );
        }
    }
    let second_start = format!(
        "<!-- steno:meetings:start -->\n{OLDER}\n<!-- steno:meetings:start -->\nHers.\n<!-- steno:meetings:end -->\n"
    );
    assert_eq!(
        ManagedBlock::merge(&line, meeting_id(), &second_start),
        second_start.replacen(OLDER, &format!("{line}\n{OLDER}"), 1)
    );
}

#[test]
fn block_lines_sort_by_date_then_text_and_undated_lines_last() {
    let lines = [
        "- 2026-01-01 b",
        "- 2026-03-01 a",
        "no date",
        "- 2026-01-01 a",
        "- 2026/09/24 x",
        "- 2026-1-01 y",
    ]
    .map(str::to_owned)
    .to_vec();
    assert_eq!(
        ManagedBlock::sorted_newest_first(lines),
        [
            "- 2026-03-01 a",
            "- 2026-01-01 a",
            "- 2026-01-01 b",
            "- 2026-1-01 y",
            "- 2026/09/24 x",
            "no date"
        ]
    );
}
