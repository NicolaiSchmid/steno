//! `SlugTests`, `TimecodeTests`, `FrontmatterTests` and `MeetingFolderTests`.

mod common;

use chrono_tz::Tz;
use common::*;
use steno_adapters::naming::{MeetingFolder, Note, Slug};
use steno_adapters::rendering::markdown_text;
use steno_adapters::rendering::{Frontmatter, FrontmatterValue, Timecode};

#[test]
fn slug_transliterates_umlauts_and_strips_other_diacritics() {
    assert_eq!(Slug::title(TITLE), "produktstrategie-90-10-roadmap-fuer-q4");
    assert_eq!(
        Slug::title("Über Äpfel, Öl und Straße"),
        "ueber-aepfel-oel-und-strasse"
    );
    assert_eq!(
        Slug::title("Jérôme trifft Zoë in São Paulo"),
        "jerome-trifft-zoe-in-sao-paulo"
    );
    assert_eq!(
        Slug::title("a\u{308}"),
        "ae",
        "a decomposed umlaut folds like the precomposed one"
    );
    assert_eq!(Slug::title("Ñandú niño"), "nandu-nino");
}

#[test]
fn slug_collapses_separators_and_trims_hyphens() {
    assert_eq!(Slug::title("  --Hello,   World!--  "), "hello-world");
    assert_eq!(Slug::title("a___b"), "a-b");
    assert_eq!(Slug::title("Weekly / Sync #3"), "weekly-sync-3");
}

#[test]
fn slug_falls_back_to_meeting_when_nothing_is_left() {
    assert_eq!(Slug::title(""), "meeting");
    assert_eq!(Slug::title("🎉🎉🎉"), "meeting");
    assert_eq!(Slug::title("日本語"), "meeting");
    assert_eq!(Slug::title("---"), "meeting");
}

#[test]
fn slug_cuts_at_the_last_hyphen_within_sixty_characters() {
    let words = (1..=20)
        .map(|n| format!("wort{n}"))
        .collect::<Vec<_>>()
        .join(" ");
    let slug = Slug::title(&words);
    assert!(slug.len() <= 60);
    assert!(!slug.ends_with('-'));
    assert_eq!(
        slug,
        "wort1-wort2-wort3-wort4-wort5-wort6-wort7-wort8-wort9-wort10"
    );
    assert_eq!(
        Slug::title(&"a".repeat(61)),
        "a".repeat(60),
        "no hyphen: a hard cut"
    );
    assert_eq!(
        Slug::title(&format!("{}-b", "a".repeat(60))),
        "a".repeat(60),
        "a cut on a hyphen keeps the word"
    );
    assert_eq!(Slug::title_with_max_length("abc-def", 5), "abc");
}

#[test]
fn slug_file_name_strips_forbidden_characters() {
    assert_eq!(Slug::file_name("Anna Müller"), "Anna Müller");
    assert_eq!(
        Slug::file_name("a/b\\c:d*e?f\"g<h>i|j#k^l[m]n"),
        "abcdefghijklmn"
    );
    assert_eq!(
        Slug::file_name("  spaced   out \t name  "),
        "spaced out name"
    );
    assert_eq!(Slug::file_name("bell\u{07}char\u{7F}"), "bellchar");
    assert_eq!(Slug::file_name("...dots..."), "dots");
    assert_eq!(Slug::file_name("..."), "Unnamed");
    assert_eq!(Slug::file_name(""), "Unnamed");
    assert_eq!(Slug::file_name(".hidden."), "hidden");
}

#[test]
fn slug_file_name_keeps_off_windows_device_names_only_where_they_are_reserved() {
    for (name, on_windows) in [
        ("Con", "Con_"),
        ("con", "con_"),
        ("PRN", "PRN_"),
        ("Aux", "Aux_"),
        ("nul", "nul_"),
        ("COM1", "COM1_"),
        ("com9", "com9_"),
        ("LPT1", "LPT1_"),
        ("lpt9", "lpt9_"),
        ("COM\u{B9}", "COM\u{B9}_"),
        ("lpt\u{B3}", "lpt\u{B3}_"),
        ("CONIN$", "CONIN$_"),
        ("conout$", "conout$_"),
        ("Conin$.md", "Conin$_.md"),
        ("nul.tar", "nul_.tar"),
        ("Con .x", "Con_ .x"),
        ("Con.", "Con_"),
        ("  Con  ", "Con_"),
    ] {
        assert_eq!(Slug::file_name_reserving(name, true), on_windows, "{name}");
        assert_eq!(
            Slug::file_name_reserving(name, false),
            on_windows.replacen('_', "", 1),
            "{name}: kept where the names are not reserved"
        );
    }
    for name in [
        "COM0",
        "LPT0",
        "COM10",
        "COM",
        "LPT",
        "Connie",
        "Console",
        "Con_",
        "Nul2",
        "CONIN",
        "CONOUT",
        "CONERR$",
        "CONIN$$",
        "aux-1",
        "x.con",
        "Anna Müller",
    ] {
        assert_eq!(
            Slug::file_name_reserving(name, true),
            Slug::file_name_reserving(name, false),
            "{name} is not a device name"
        );
    }
    assert_eq!(
        Slug::file_name("Con"),
        if cfg!(windows) { "Con_" } else { "Con" },
        "the rule applies on Windows only"
    );
}

#[test]
fn a_wikilink_to_a_renamed_device_name_shows_the_name_as_written() {
    if cfg!(windows) {
        assert_eq!(markdown_text::wikilink("Con", None), "[[Con_|Con]]");
        assert_eq!(markdown_text::wikilink("Con", Some("C")), "[[Con_|C]]");
    } else {
        assert_eq!(markdown_text::wikilink("Con", None), "[[Con]]");
    }
    assert_eq!(
        markdown_text::wikilink("Anna Müller", None),
        "[[Anna Müller]]"
    );
}

#[test]
fn slug_is_independent_of_the_process_locale() {
    for title in ["Straße İstanbul", "ÄÖÜ äöü", "Ærø Œuvre", "İ ı I i"] {
        let slug = Slug::title(title);
        assert!(slug.is_ascii(), "{title} -> {slug}");
        assert_eq!(slug, Slug::title(title));
    }
    assert_eq!(Slug::title("Straße İstanbul"), "strasse-istanbul");
}

#[test]
fn timecode_clock_floors_to_whole_seconds() {
    assert_eq!(Timecode::clock(0.0), "00:00:00");
    assert_eq!(Timecode::clock(754.9), "00:12:34");
    assert_eq!(Timecode::clock(3661.0), "01:01:01");
    assert_eq!(
        Timecode::clock(36_000.0 * 3.0),
        "30:00:00",
        "hours are not capped at two digits' worth"
    );
    assert_eq!(Timecode::clock(-5.0), "00:00:00");
    assert_eq!(Timecode::clock(f64::NAN), "00:00:00");
}

#[test]
fn timecode_vtt_rounds_to_milliseconds() {
    assert_eq!(Timecode::vtt(0.0), "00:00:00.000");
    assert_eq!(Timecode::vtt(754.567), "00:12:34.567");
    assert_eq!(Timecode::vtt(0.0005), "00:00:00.001");
    assert_eq!(Timecode::vtt(59.9996), "00:01:00.000");
    assert_eq!(Timecode::vtt(4.2), "00:00:04.200");
}

#[test]
fn timecode_pads_to_the_requested_width() {
    assert_eq!(Timecode::pad(7, 2), "07");
    assert_eq!(Timecode::pad(123, 2), "123");
    assert_eq!(Timecode::pad(5, 3), "005");
}

fn sample_frontmatter() -> Frontmatter {
    let mut frontmatter = Frontmatter::new(BERLIN);
    let string = |text: &str| FrontmatterValue::String(text.to_owned());
    frontmatter.append("title", string(TITLE));
    frontmatter.append("colon", string("key: value"));
    frontmatter.append("backslash", string("C:\\Users\\anna"));
    frontmatter.append("dash", string("- not a list"));
    frontmatter.append("hash", string("tag #1 here"));
    frontmatter.append("answer", string("yes"));
    frontmatter.append("date_like", string("2026-09-24"));
    frontmatter.append("empty", string(""));
    frontmatter.append("control", string("bell\u{07}tab\tnewline\ndel\u{7F}"));
    frontmatter.append("date", FrontmatterValue::DateTime(started_at()));
    frontmatter.append("day", FrontmatterValue::Date(started_at()));
    frontmatter.append("duration", FrontmatterValue::Int(90));
    frontmatter.append("done", FrontmatterValue::Bool(false));
    frontmatter.append(
        "participants",
        FrontmatterValue::List(vec!["[[Anna Müller]]".to_owned(), "Speaker 2".to_owned()]),
    );
    frontmatter.append("nothing", FrontmatterValue::List(vec![]));
    frontmatter.append(
        "tags",
        FrontmatterValue::List(
            [
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
            ]
            .iter()
            .filter_map(|tag| markdown_text::tag(tag))
            .collect(),
        ),
    );
    frontmatter
}

#[test]
fn frontmatter_encodes_every_value_kind_and_matches_the_golden() {
    let encoded = sample_frontmatter().encoded();
    assert!(encoded.starts_with("---\n"));
    assert!(encoded.ends_with("\n---\n"));
    assert!(encoded.contains("control: \"bell\\u0007tab\\tnewline\\ndel\\u007F\"\n"));
    assert!(encoded.contains("date: 2026-09-24T14:00:00\n"));
    assert!(encoded.contains("day: 2026-09-24\n"));
    assert!(encoded.contains("nothing: []\n"));
    assert_matches_golden(&encoded, "snapshots/obsidian/frontmatter.md");
}

#[test]
fn frontmatter_quoted_escapes_only_what_yaml_needs() {
    assert_eq!(Frontmatter::quoted("plain"), "\"plain\"");
    assert_eq!(Frontmatter::quoted("a\"b"), "\"a\\\"b\"");
    assert_eq!(Frontmatter::quoted("a\\b"), "\"a\\\\b\"");
    assert_eq!(Frontmatter::quoted("ü 日本 🎉"), "\"ü 日本 🎉\"");
    assert_eq!(Frontmatter::quoted("\r\n"), "\"\\r\\n\"");
    assert_eq!(Frontmatter::quoted("\u{1B}"), "\"\\u001B\"");
    assert_eq!(
        Frontmatter::quoted("a\u{85}b\u{9F}c"),
        "\"a\\u0085b\\u009Fc\""
    );
    assert_eq!(
        Frontmatter::quoted("\u{2028}\u{2029}"),
        "\"\\u2028\\u2029\""
    );
    assert_eq!(Frontmatter::quoted("\u{FEFF}title"), "\"\\uFEFFtitle\"");
    assert_eq!(
        Frontmatter::quoted("\u{A0}\u{2027}"),
        "\"\u{A0}\u{2027}\"",
        "neighbours stay"
    );
}

#[test]
fn meeting_folder_uses_the_start_date_in_the_given_time_zone() {
    let meeting = meeting();
    assert_eq!(MeetingFolder::path(&meeting, BERLIN), FOLDER);
    assert_eq!(MeetingFolder::basename(&meeting, Tz::UTC), FOLDER_SLUG);
}

#[test]
fn meeting_folder_date_follows_the_time_zone_across_midnight_and_in_winter() {
    let mut meeting = meeting();
    meeting.started_at = epoch(1_790_290_800); // 2026-09-24T23:00:00Z
    meeting.title = "Late".to_owned();
    assert_eq!(
        MeetingFolder::path(&meeting, Tz::UTC),
        "Meetings/2026-09-24-late"
    );
    assert_eq!(
        MeetingFolder::path(&meeting, BERLIN),
        "Meetings/2026-09-25-late"
    );
    meeting.started_at = epoch(1_796_166_000); // 2026-12-01T23:00:00Z
    meeting.title = "Winter".to_owned();
    assert_eq!(
        MeetingFolder::path(&meeting, Tz::UTC),
        "Meetings/2026-12-01-winter"
    );
    assert_eq!(
        MeetingFolder::path(&meeting, BERLIN),
        "Meetings/2026-12-02-winter",
        "CET is one hour ahead, not two"
    );
    meeting.title = "???".to_owned();
    assert_eq!(
        MeetingFolder::basename(&meeting, Tz::UTC),
        "2026-12-01-meeting"
    );
}

#[test]
fn note_names_are_the_wikilink_targets_and_files_add_the_extension() {
    assert_eq!(
        MeetingFolder::note_name(Note::Folder, FOLDER_SLUG),
        FOLDER_SLUG
    );
    assert_eq!(
        MeetingFolder::note_name(Note::Transcript, FOLDER_SLUG),
        format!("{FOLDER_SLUG} - Transcript")
    );
    assert_eq!(
        MeetingFolder::note_name(Note::Tasks, FOLDER_SLUG),
        format!("{FOLDER_SLUG} - Tasks")
    );
    for note in Note::ALL {
        assert_eq!(
            MeetingFolder::note_file(note, FOLDER_SLUG),
            format!("{}.md", MeetingFolder::note_name(note, FOLDER_SLUG))
        );
    }
    assert_eq!(MeetingFolder::VTT, "transcript.vtt");
    assert_eq!(MeetingFolder::JSON, "meeting.json");
    assert_eq!(MeetingFolder::audio_file("m4a"), "audio.m4a");
    assert_eq!(MeetingFolder::audio_file(""), "audio");
}
