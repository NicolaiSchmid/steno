//! The scope's fixed meeting folder and the one spelling of every file in it.
//! Swift: `Sources/StenoAdapters/Naming/MeetingFolder.swift`.

use chrono_tz::Tz;
use steno_core::Meeting;

use super::Slug;
use crate::rendering::date_text;

/// The three notes in a meeting folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Note {
    Folder,
    Transcript,
    Tasks,
}

impl Note {
    pub const ALL: [Note; 3] = [Note::Folder, Note::Transcript, Note::Tasks];
}

/// The scope's fixed meeting folder and the one spelling of every file in it:
///
/// ```text
/// <root>/Meetings/<yyyy-MM-dd>-<slug>/
///   <yyyy-MM-dd>-<slug>.md                folder note
///   <yyyy-MM-dd>-<slug> - Transcript.md
///   <yyyy-MM-dd>-<slug> - Tasks.md
///   transcript.vtt
///   meeting.json
///   audio.<ext>                           only with includeAudio
/// ```
///
/// The date is `started_at` in the given time zone; the folder's basename is
/// the slug every note inside it is named after. Renderers name notes and
/// links through it, destinations place files by it.
pub struct MeetingFolder;

impl MeetingFolder {
    pub const ROOT: &'static str = "Meetings";
    pub const VTT: &'static str = "transcript.vtt";
    pub const JSON: &'static str = "meeting.json";

    /// `"2026-09-24-produktstrategie-90-10-roadmap-fuer-q4"`.
    #[must_use]
    pub fn basename(meeting: &Meeting, time_zone: Tz) -> String {
        format!(
            "{}-{}",
            date_text::day(meeting.started_at, time_zone),
            Slug::title(&meeting.title)
        )
    }

    /// `"Meetings/2026-09-24-produktstrategie-90-10-roadmap-fuer-q4"`.
    #[must_use]
    pub fn path(meeting: &Meeting, time_zone: Tz) -> String {
        format!("{}/{}", Self::ROOT, Self::basename(meeting, time_zone))
    }

    /// The note's name without extension, which is what a wikilink targets:
    /// `"<slug>"`, `"<slug> - Transcript"`, `"<slug> - Tasks"`.
    #[must_use]
    pub fn note_name(note: Note, slug: &str) -> String {
        match note {
            Note::Folder => slug.to_owned(),
            Note::Transcript => format!("{slug} - Transcript"),
            Note::Tasks => format!("{slug} - Tasks"),
        }
    }

    /// [`MeetingFolder::note_name`] with `.md`.
    #[must_use]
    pub fn note_file(note: Note, slug: &str) -> String {
        format!("{}.md", Self::note_name(note, slug))
    }

    /// `"audio.m4a"` for the AAC mixdown; the extension follows the mixdown
    /// file so the bytes and the name never disagree.
    #[must_use]
    pub fn audio_file(file_extension: &str) -> String {
        if file_extension.is_empty() {
            "audio".to_owned()
        } else {
            format!("audio.{file_extension}")
        }
    }
}
