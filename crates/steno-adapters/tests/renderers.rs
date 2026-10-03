//! The renderers against the Swift goldens in `Tests/Fixtures/snapshots/obsidian`
//! and the behaviour tests of `FolderNoteRendererTests`,
//! `TranscriptMarkdownRendererTests`, `TasksMarkdownRendererTests`,
//! `WebVTTRendererTests` and `MeetingJSONRendererTests`.

mod common;

use chrono_tz::Tz;
use common::*;
use steno_adapters::naming::MeetingFolder;
use steno_adapters::rendering::{ArtifactRenderer, RenderOptions, RenderedArtifactKind};
use steno_core::summary;
use steno_core::{MeetingExport, MeetingTask, TaskPriority, TranscriptSegment};

fn renderer() -> ArtifactRenderer {
    ArtifactRenderer::new()
}

#[test]
fn folder_note_matches_the_goldens_in_every_variant() {
    let export = export();
    assert_matches_golden(
        &renderer().render_folder_note(&export, &plain(), None),
        "snapshots/obsidian/folder-note-plain-utc.md",
    );
    assert_matches_golden(
        &renderer().render_folder_note(&export, &plain_berlin(), None),
        "snapshots/obsidian/folder-note-plain-berlin.md",
    );
    assert_matches_golden(
        &renderer().render_folder_note(&export, &wikilink(), None),
        "snapshots/obsidian/folder-note-wikilink-berlin.md",
    );
    assert_matches_golden(
        &renderer().render_folder_note(&export, &wikilink_utc(), None),
        "snapshots/obsidian/folder-note-wikilink-utc.md",
    );
}

#[test]
fn folder_note_frontmatter_carries_the_scope_fields_plus_title() {
    let note = renderer().render_folder_note(&export(), &wikilink(), None);
    assert!(note.starts_with("---\ntitle: \"Produktstrategie: \\\"90/10\\\" & Roadmap für Q4\"\n"));
    assert!(note.contains("\ndate: 2026-09-24T14:00:00\n"));
    assert!(note.contains("\nduration: 90\n"));
    assert!(note.contains(
        "\nparticipants:\n  - \"[[Anna Müller]]\"\n  - \"Jérôme Dupont\"\n  - \"[[Nicolai Schmid]]\"\n  - \"Speaker 2\"\n"
    ));
    assert!(note.contains("\ntags:\n  - \"meeting\"\n  - \"Kunde-ACME\"\n  - \"q4\"\n"));
    assert!(note.contains("\nsource: \"mac-call\"\n"));
    assert!(note.contains("\ntemplate: \"default\"\n"));
    assert!(note.contains("\nlanguage: \"de\"\n"));
    assert!(note.contains("\nsteno_id: \"00000000-0000-0000-0000-000000000001\"\n---\n"));
}

#[test]
fn folder_note_body_has_title_info_line_summary_decisions_and_scratchpad() {
    let export = export();
    let note = renderer().render_folder_note(&export, &wikilink(), None);
    assert!(note.contains("\n# Produktstrategie: \"90/10\" & Roadmap für Q4\n"));
    assert!(note.contains(&format!(
        "\n2026-09-24 14:00–15:30 · 1 h 30 min · Mac call · [[{FOLDER_SLUG} - Transcript|Transcript]] · [[{FOLDER_SLUG} - Tasks|Tasks]]\n"
    )));
    let rendered = summary::render(&export);
    assert!(note.contains("\n### Executive Summary\n\n- **Fokus**: "));
    assert!(!note.contains("\n## Executive Summary\n"));
    assert!(
        rendered.contains("**Anna Müller** setzt 90 Prozent"),
        "a confirmed speaker is named in bold"
    );
    assert!(
        rendered.contains("Speaker 2 verteilt"),
        "an unresolved speaker keeps its label"
    );
    assert!(note.contains(
        "\n## Decisions\n\n- 90/10-Aufteilung wird umgesetzt.\n- Roadmap-Review am 15. Oktober.\n"
    ));
    assert!(note.ends_with(
        "\n## Scratchpad\n\nNachfassen wegen Budget.\n\n---\n\n## Summary\n\nNicht vergessen: Jérôme fragen.\n"
    ));
}

#[test]
fn the_info_line_end_adds_the_raw_duration_like_swift() {
    let mut export = export();
    export.meeting.duration = 5399.9995;
    let note = renderer().render_folder_note(&export, &wikilink(), None);
    assert!(
        note.contains("\n2026-09-24 14:00–15:29 · 1 h 30 min · "),
        "half a millisecond under the boundary stays at 15:29, not rounded onto 15:30: {note}"
    );
    for duration in [-5.0, f64::NAN, f64::INFINITY, 1e300] {
        export.meeting.duration = duration;
        let note = renderer().render_folder_note(&export, &wikilink(), None);
        assert!(
            note.contains("\n2026-09-24 14:00–14:00 · "),
            "{duration}: nothing is added: {note}"
        );
    }
}

#[test]
fn plain_style_links_nothing_and_uses_markdown_links_for_the_notes() {
    let note = renderer().render_folder_note(&export(), &plain(), None);
    assert!(!note.contains("[["));
    assert!(note.contains("\n  - \"Anna Müller\"\n"));
    assert!(note.contains(&format!(
        "\n2026-09-24 12:00–13:30 · 1 h 30 min · Mac call · [Transcript](<{FOLDER_SLUG} - Transcript.md>) · [Tasks](<{FOLDER_SLUG} - Tasks.md>)\n"
    )));
    assert!(note.contains("\ndate: 2026-09-24T12:00:00\n"));
}

#[test]
fn empty_sections_are_omitted_and_the_folder_slug_is_pinned() {
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
    assert!(
        note.contains("\nduration: 46\n"),
        "whole minutes, rounded up"
    );
    assert!(note.contains(" · 46 min · "));
    assert!(note.contains("[[2026-09-24-pinned - Transcript|Transcript]]"));
    export.meeting.duration = 7200.0;
    assert!(
        renderer()
            .render_folder_note(&export, &wikilink(), None)
            .contains(" · 2 h · ")
    );
    export.meeting.duration = 0.0;
    assert!(
        renderer()
            .render_folder_note(&export, &wikilink(), None)
            .contains(" · 0 min · ")
    );
}

#[test]
fn meeting_files_and_person_pages_are_rendered_through_two_seams() {
    let export = export();
    let artifacts = renderer()
        .render_meeting_files(&export, &wikilink(), None)
        .unwrap();
    assert_eq!(
        artifacts
            .iter()
            .map(|a| a.file_name.as_str())
            .collect::<Vec<_>>(),
        [
            "meeting.json".to_owned(),
            format!("{FOLDER_SLUG}.md"),
            format!("{FOLDER_SLUG} - Transcript.md"),
            format!("{FOLDER_SLUG} - Tasks.md"),
            "transcript.vtt".to_owned(),
        ]
    );
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
    let pages = renderer().render_person_pages(&export, &wikilink(), None);
    assert_eq!(
        pages
            .iter()
            .map(|p| p.file_name.as_str())
            .collect::<Vec<_>>(),
        ["Anna Müller.md", "Nicolai Schmid.md"]
    );
    assert!(
        pages.iter().all(|page| page.page.contains(&page.line)),
        "the page embeds the line"
    );
    assert_eq!(
        renderer().render_person_pages(&export, &plain(), None),
        vec![]
    );
    let plain_pages = RenderOptions {
        person_pages: true,
        ..RenderOptions::PLAIN
    };
    assert_eq!(
        renderer()
            .render_person_pages(&export, &plain_pages, None)
            .len(),
        2
    );
}

#[test]
fn hostile_title_and_tags_stay_inside_their_scalars() {
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
    assert!(
        note.starts_with(
            "---\ntitle: \"- yes: #no \\\"quoted\\\" \\\\ end\\nsecond line\"\ndate: "
        ),
        "the title is one double-quoted scalar"
    );
    let tags = [
        "meeting",
        "x",
        "yes",
        "ab",
        "with-space",
        "C/D_e",
        "kunde/acme",
    ];
    let list = tags.iter().fold(String::new(), |mut list, tag| {
        use std::fmt::Write as _;
        let _ = writeln!(list, "  - \"{tag}\"");
        list
    });
    let expected = format!("\ntags:\n{list}source:");
    assert!(
        note.contains(&expected),
        "tags lose #, :, spaces and symbols; empty ones are dropped"
    );
    assert!(
        note.contains("\n# - yes: #no \"quoted\" \\ end second line\n"),
        "H1 is one line"
    );
    let lines: Vec<&str> = note.split('\n').collect();
    let fence = lines[1..].iter().position(|line| *line == "---").unwrap() + 1;
    for line in &lines[1..fence] {
        if !line.starts_with("  - ") {
            assert!(line.contains(": ") || line.ends_with(':'), "{line}");
        }
    }
    assert_eq!(
        MeetingFolder::basename(&export.meeting, BERLIN),
        "2026-09-24-yes-no-quoted-end-second-line"
    );
}

#[test]
fn person_page_file_names_are_the_wikilink_targets() {
    let mut export = export();
    export.persons[0].display_name = "Anna/Müller: <CEO> [Acme]".to_owned();
    export.participants[0].display_name = export.persons[0].display_name.clone();
    let artifacts = renderer()
        .render_meeting_files(&export, &wikilink(), None)
        .unwrap();
    let page = &renderer().render_person_pages(&export, &wikilink(), None)[0];
    assert_eq!(page.file_name, "AnnaMüller CEO Acme.md");
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
    assert!(text(RenderedArtifactKind::FolderNote).contains("  - \"[[AnnaMüller CEO Acme]]\"\n"));
    assert!(
        text(RenderedArtifactKind::Transcript)
            .contains("\n## [[AnnaMüller CEO Acme]] — 00:00:04\n")
    );
    assert!(text(RenderedArtifactKind::Tasks).contains(" [[AnnaMüller CEO Acme]] "));
    assert!(
        page.page.contains("# Anna/Müller: <CEO> [Acme]\n"),
        "the H1 keeps the real name"
    );
}

// Transcript

#[test]
fn transcript_matches_the_goldens() {
    let export = export();
    assert_matches_golden(
        &renderer().render_transcript(&export, &plain()),
        "snapshots/obsidian/transcript-plain.md",
    );
    assert_matches_golden(
        &renderer().render_transcript(&export, &wikilink()),
        "snapshots/obsidian/transcript-wikilink.md",
    );
}

#[test]
fn transcript_groups_consecutive_segments_into_turns_and_breaks_paragraphs_at_gaps() {
    let note = renderer().render_transcript(&export(), &wikilink());
    assert!(note.starts_with(
        "---\ntitle: \"Produktstrategie: \\\"90/10\\\" & Roadmap für Q4 — Transcript\"\ntype: \"transcript\"\nsteno_id: \"00000000-0000-0000-0000-000000000001\"\n---\n"
    ));
    assert!(note.contains("\n## [[Nicolai Schmid]] — 00:00:00\n\nGuten Morgen zusammen, fangen wir mit der Roadmap an.\n"));
    assert!(note.contains(
        "\n## [[Anna Müller]] — 00:00:04\n\nGern. Ich habe die Zahlen für Q4 dabei. Der Kern bekommt 90 Prozent, der Rest 10.\n\nDas ACME-Angebot muss bis zum 1. Oktober raus.\n"
    ));
    assert!(
        note.contains("\n## Speaker 2 — 00:00:30\n\nIch kann das Protokoll verteilen. Okay.\n")
    );
    assert!(note.contains("\n## Unknown — 00:00:37\n\n\\- Notiz: Einwurf aus dem Off.\n"));
    assert!(note.contains("\n\\# Punkt eins: Budget <Kern> & Rest --> offen\n"));
    assert!(note.contains("\n\\1. Wir starten im Oktober.\n"));
    assert!(note.contains("\n\\> Zitat aus dem Kundenbrief.\n"));
    assert_eq!(
        note.matches("\n## ").count(),
        11,
        "fourteen segments form eleven turns"
    );
    assert!(note.ends_with("\n## [[Nicolai Schmid]] — 00:01:10\n\nDanke, bis nächste Woche.\n"));
}

#[test]
fn transcript_plain_style_writes_names_without_links() {
    let note = renderer().render_transcript(&export(), &plain());
    assert!(!note.contains("[["));
    assert!(note.contains("\n## Anna Müller — 00:00:04\n"));
}

#[test]
fn an_empty_transcript_says_so_and_a_blank_segment_is_skipped() {
    let mut export = export();
    export.segments = vec![];
    assert!(
        renderer()
            .render_transcript(&export, &plain())
            .ends_with("— Transcript\n\nNo transcript.\n")
    );
    let segment = |n: u32, start: f64, end: f64, text: &str| TranscriptSegment {
        id: uuid(n),
        meeting_id: export.meeting.id,
        start,
        end,
        speaker_id: Some(speaker_one_id()),
        lane: steno_core::AudioLane::System,
        text: text.to_owned(),
        raw_text: String::new(),
    };
    export.segments = vec![
        segment(1, 1.0, 2.0, "  \n "),
        segment(2, 2.0, 3.0, "Line one\nline two"),
    ];
    let note = renderer().render_transcript(&export, &plain());
    assert!(note.ends_with("\n## Anna Müller — 00:00:02\n\nLine one line two\n"));
}

// Tasks

#[test]
fn tasks_match_the_goldens() {
    let export = export();
    assert_matches_golden(
        &renderer().render_tasks(&export, &plain()),
        "snapshots/obsidian/tasks-plain.md",
    );
    assert_matches_golden(
        &renderer().render_tasks(&export, &wikilink()),
        "snapshots/obsidian/tasks-wikilink.md",
    );
    assert_matches_golden(
        &renderer().render_tasks(&export, &wikilink_tag()),
        "snapshots/obsidian/tasks-wikilink-tag.md",
    );
}

fn task_lines(note: &str) -> Vec<&str> {
    note.split('\n')
        .filter(|line| line.starts_with("- ["))
        .collect()
}

#[test]
fn task_lines_follow_the_verified_field_order() {
    let note = renderer().render_tasks(&export(), &wikilink_tag());
    assert_eq!(
        task_lines(&note),
        [
            "- [ ] Angebot an ACME schicken [[Anna Müller]] #task \u{23EB} \u{1F4C5} 2026-10-01",
            "- [ ] Roadmap-Folien aktualisieren [[Nicolai Schmid]] #task \u{1F4C5} 2026-10-15",
            "- [x] Protokoll verteilen #task \u{1F53D}",
            "- [ ] Budget freigeben lassen Jérôme Dupont #task",
        ]
    );
    assert!(note.ends_with("\n\nEdit tasks in Steno; this file is rewritten on re-export.\n"));
    assert!(note.starts_with(
        "---\ntitle: \"Produktstrategie: \\\"90/10\\\" & Roadmap für Q4 — Tasks\"\ntype: \"tasks\"\n"
    ));
    assert!(!note.contains('\u{FE0F}'));
    assert!(!note.contains('\u{00A0}'));
}

#[test]
fn plain_style_tasks_carry_no_tag_and_an_empty_list_says_no_tasks() {
    let mut export = export();
    let plain_note = renderer().render_tasks(&export, &plain());
    assert!(
        plain_note.contains(
            "\n- [ ] Angebot an ACME schicken Anna Müller \u{23EB} \u{1F4C5} 2026-10-01\n"
        )
    );
    assert!(!plain_note.contains("#task"));
    export.tasks = vec![];
    assert!(renderer().render_tasks(&export, &plain()).ends_with(
        "— Tasks\n\nNo tasks.\n\nEdit tasks in Steno; this file is rewritten on re-export.\n"
    ));
}

fn bare_task(
    n: u32,
    export: &MeetingExport,
    text: &str,
    assignee: Option<&str>,
    priority: TaskPriority,
) -> MeetingTask {
    MeetingTask {
        id: uuid(n),
        meeting_id: export.meeting.id,
        text: text.to_owned(),
        assignee_person_id: None,
        assignee_name: assignee.map(str::to_owned),
        priority,
        due_date: None,
        done: false,
    }
}

#[test]
fn an_assignee_named_by_cluster_label_resolves_to_the_person() {
    let mut export = export();
    export.tasks = vec![
        bare_task(
            60,
            &export,
            "Nachfassen",
            Some("Speaker 1"),
            TaskPriority::Normal,
        ),
        bare_task(
            61,
            &export,
            "Offen lassen",
            Some("Speaker 2"),
            TaskPriority::Normal,
        ),
    ];
    let note = renderer().render_tasks(&export, &wikilink());
    assert!(
        note.contains("\n- [ ] Nachfassen [[Anna Müller]]\n"),
        "Speaker 1 is Anna"
    );
    assert!(
        note.contains("\n- [ ] Offen lassen Speaker 2\n"),
        "Speaker 2 resolved to nobody"
    );
}

#[test]
fn the_task_tag_is_sanitised() {
    let options = RenderOptions {
        task_tag: Some("#steno tasks".to_owned()),
        time_zone: Tz::UTC,
        ..wikilink()
    };
    assert!(
        renderer()
            .render_tasks(&export(), &options)
            .contains(" #steno-tasks ")
    );
}

#[test]
fn every_priority_on_a_bare_task_and_the_done_marker() {
    let mut export = export();
    export.tasks = vec![
        bare_task(80, &export, "Nur low", None, TaskPriority::Low),
        bare_task(81, &export, "Nur normal", None, TaskPriority::Normal),
        bare_task(82, &export, "Nur high", None, TaskPriority::High),
        MeetingTask {
            due_date: Some(due_october_first()),
            done: true,
            ..bare_task(90, &export, "Erledigt", None, TaskPriority::High)
        },
    ];
    let note = renderer().render_tasks(&export, &wikilink_tag());
    assert_eq!(
        task_lines(&note),
        [
            "- [ ] Nur low #task \u{1F53D}",
            "- [ ] Nur normal #task",
            "- [ ] Nur high #task \u{23EB}",
            "- [x] Erledigt #task \u{23EB} \u{1F4C5} 2026-10-01",
        ]
    );
    let plain_note = renderer().render_tasks(&export, &plain());
    assert!(plain_note.contains("\n- [ ] Nur normal\n"));
    assert!(
        !plain_note.contains('\u{2705}'),
        "done writes [x] and no completion date"
    );
}

#[test]
fn a_due_date_follows_the_options_time_zone() {
    let mut export = export();
    // 2026-10-01T23:30:00Z: still the 1st in UTC, already the 2nd in Berlin.
    export.tasks = vec![MeetingTask {
        due_date: Some(epoch(1_790_897_400)),
        ..bare_task(81, &export, "Spät", None, TaskPriority::Normal)
    }];
    assert!(
        renderer()
            .render_tasks(&export, &plain())
            .contains("\n- [ ] Spät \u{1F4C5} 2026-10-01\n")
    );
    assert!(
        renderer()
            .render_tasks(&export, &plain_berlin())
            .contains("\n- [ ] Spät \u{1F4C5} 2026-10-02\n")
    );
}

// WebVTT

#[test]
fn vtt_matches_the_golden() {
    assert_matches_golden(
        &renderer().render_vtt(&export()),
        "snapshots/obsidian/transcript.vtt",
    );
}

#[test]
fn the_vtt_header_note_precedes_cues_in_start_order_with_voice_spans() {
    let vtt = renderer().render_vtt(&export());
    assert!(vtt.starts_with(
        "WEBVTT - Steno 00000000-0000-0000-0000-000000000001\n\nNOTE\nProduktstrategie: \"90/10\" & Roadmap für Q4\n2026-09-24T12:00:00Z\n\n00:00:00.000 --> 00:00:04.200\n<v Nicolai Schmid>Guten Morgen zusammen, fangen wir mit der Roadmap an.\n\n00:00:04.500 --> 00:00:09.800\n<v Anna Müller>Gern. Ich habe die Zahlen für Q4 dabei.\n"
    ));
    assert_eq!(vtt.matches(" --> ").count(), 14, "fourteen cues");
    let starts: Vec<&str> = vtt
        .split('\n')
        .filter(|line| line.contains(" --> "))
        .map(|line| &line[..12])
        .collect();
    let mut sorted = starts.clone();
    sorted.sort_unstable();
    assert_eq!(starts, sorted);
    assert!(vtt.contains("\n<v Speaker 2>Ich kann das Protokoll verteilen.\n"));
    assert!(vtt.contains("\n<v Unknown>- Notiz: Einwurf aus dem Off.\n"));
    assert!(vtt.ends_with(
        "\n00:01:10.500 --> 00:01:12.000\n<v Nicolai Schmid>Danke, bis nächste Woche.\n"
    ));
    assert!(vtt.contains("\n00:00:36.000 --> 00:00:36.001\n<v Speaker 2>Okay.\n"));
    assert!(
        vtt.contains("\n<v Nicolai Schmid># Punkt eins: Budget &lt;Kern&gt; &amp; Rest  offen\n")
    );
}

#[test]
fn a_voice_name_cannot_close_the_span_or_carry_an_ampersand() {
    let mut export = export();
    export.persons[1].display_name = "Tom <Tommy> & Söhne > GmbH\nBerlin".to_owned();
    export.meeting.title = "Q4 --> Q1 & <Plan>".to_owned();
    let vtt = renderer().render_vtt(&export);
    assert!(vtt.contains(
        "\n00:00:00.000 --> 00:00:04.200\n<v Tom <Tommy  Söhne  GmbH Berlin>Guten Morgen zusammen, fangen wir mit der Roadmap an.\n"
    ));
    assert!(
        vtt.contains("\nNOTE\nQ4  Q1 & <Plan>\n"),
        "the NOTE block only loses -->"
    );
    let mut cues = 0;
    for line in vtt.split('\n').filter(|line| line.starts_with("<v ")) {
        let close = line.find('>').unwrap();
        assert!(!line[3..close].contains('&'), "{line}");
        cues += 1;
    }
    assert_eq!(cues, 14);
    assert!(!vtt.contains("\n\n\n"), "no blank cue blocks");
}

// meeting.json

#[test]
fn meeting_json_matches_the_golden_and_the_fixture_file() {
    let export = export();
    let data = renderer().render_json(&export).unwrap();
    assert_eq!(data, fixture("snapshots/obsidian/meeting.json"));
    assert_eq!(data, fixture("meetings/produktstrategie.json"));
    let decoded: MeetingExport = serde_json::from_slice(&data).unwrap();
    assert_eq!(
        decoded, export,
        "the fixture file on disk is the fixture meeting"
    );
    assert_eq!(
        renderer().render_json(&decoded).unwrap(),
        data,
        "decodes back and encodes byte-identically"
    );
}

#[test]
fn meeting_json_shape_is_camel_case_without_embeddings() {
    let text = String::from_utf8(renderer().render_json(&export()).unwrap()).unwrap();
    assert!(!text.contains("embedding"));
    assert!(text.contains("\"schemaVersion\" : 1"));
    assert!(text.contains("\"mixdownURL\" : \"file:///tmp/steno/"));
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix('"')
            && let Some(end) = rest.find("\" :")
        {
            let key = &rest[..end];
            assert!(!key.contains('_'), "{key} is not camelCase");
            assert!(
                key.chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            );
        }
    }
}
