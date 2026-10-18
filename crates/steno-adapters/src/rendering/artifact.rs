//! The renderer facade and the files it produces.

use chrono_tz::Tz;
use serde_json::Value;
use steno_core::{MeetingExport, TranscriptSegment};

use super::{
    FolderNoteRenderer, Frontmatter, FrontmatterValue, PersonPageRenderer, RenderOptions,
    TasksMarkdownRenderer, TranscriptMarkdownRenderer, WebVttRenderer, markdown_text,
};
use crate::naming::{MeetingFolder, Note};

/// Which file of the meeting folder a [`RenderedArtifact`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderedArtifactKind {
    Json,
    FolderNote,
    Transcript,
    Tasks,
    Vtt,
}

/// One rendered file of the meeting folder; `file_name` is relative to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedArtifact {
    pub kind: RenderedArtifactKind,
    pub file_name: String,
    pub data: Vec<u8>,
}

/// One person's page for this meeting, `file_name` relative to the people
/// folder: `page` when created new, `line` to merge into an existing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonPage {
    pub file_name: String,
    pub page: String,
    pub line: String,
}

/// Pure renderers from the canonical model to bytes. No I/O, no clock;
/// equal inputs give equal bytes. On re-export the pinned folder basename
/// comes in as `folder_slug` so note names stay put.
///
/// ```
/// use steno_adapters::rendering::{ArtifactRenderer, RenderOptions};
/// use steno_core::testing::sample_data;
///
/// let note =
///     ArtifactRenderer::new().render_folder_note(&sample_data::export(), &RenderOptions::PLAIN, None);
/// assert!(note.starts_with("---\ntitle: "));
/// assert!(note.contains("\n## Summary\n"));
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct ArtifactRenderer;

impl ArtifactRenderer {
    /// Bumped whenever the Mac's bytes change; recorded in every receipt
    /// and pinned by `Tests/Fixtures/snapshots/obsidian/VERSION`.
    pub const VERSION: i64 = 2;

    #[must_use]
    pub fn new() -> Self {
        ArtifactRenderer
    }

    /// The five files of the meeting folder. `meeting.json` comes first, as
    /// that file alone marks a folder as this meeting's crashed attempt.
    pub fn render_meeting_files(
        &self,
        export: &MeetingExport,
        options: &RenderOptions,
        folder_slug: Option<&str>,
    ) -> Result<Vec<RenderedArtifact>, serde_json::Error> {
        let slug = Self::slug(export, options, folder_slug);
        Ok(vec![
            RenderedArtifact {
                kind: RenderedArtifactKind::Json,
                file_name: MeetingFolder::JSON.to_owned(),
                data: self.render_json(export)?,
            },
            RenderedArtifact {
                kind: RenderedArtifactKind::FolderNote,
                file_name: MeetingFolder::note_file(Note::Folder, &slug),
                data: self
                    .render_folder_note(export, options, Some(&slug))
                    .into_bytes(),
            },
            RenderedArtifact {
                kind: RenderedArtifactKind::Transcript,
                file_name: MeetingFolder::note_file(Note::Transcript, &slug),
                data: self.render_transcript(export, options).into_bytes(),
            },
            RenderedArtifact {
                kind: RenderedArtifactKind::Tasks,
                file_name: MeetingFolder::note_file(Note::Tasks, &slug),
                data: self.render_tasks(export, options).into_bytes(),
            },
            RenderedArtifact {
                kind: RenderedArtifactKind::Vtt,
                file_name: MeetingFolder::VTT.to_owned(),
                data: self.render_vtt(export).into_bytes(),
            },
        ])
    }

    /// One page per person in export order when person pages are on.
    #[must_use]
    pub fn render_person_pages(
        &self,
        export: &MeetingExport,
        options: &RenderOptions,
        folder_slug: Option<&str>,
    ) -> Vec<PersonPage> {
        if !options.person_pages {
            return Vec::new();
        }
        let slug = Self::slug(export, options, folder_slug);
        let renderer = PersonPageRenderer {
            export,
            options,
            folder_slug: &slug,
        };
        export
            .persons
            .iter()
            .map(|person| renderer.page(person))
            .collect()
    }

    /// Frontmatter, title, info line, summary, and decisions and scratchpad
    /// when present.
    #[must_use]
    pub fn render_folder_note(
        &self,
        export: &MeetingExport,
        options: &RenderOptions,
        folder_slug: Option<&str>,
    ) -> String {
        let slug = Self::slug(export, options, folder_slug);
        FolderNoteRenderer {
            export,
            options,
            folder_slug: &slug,
        }
        .render()
    }

    /// One `## Name — 00:12:34` header per turn, paragraphs split at gaps of
    /// three seconds or more.
    #[must_use]
    pub fn render_transcript(&self, export: &MeetingExport, options: &RenderOptions) -> String {
        TranscriptMarkdownRenderer { export, options }.render()
    }

    /// Obsidian Tasks lines: `- [ ] text [[Assignee]] #tag ⏫ 📅 YYYY-MM-DD`.
    #[must_use]
    pub fn render_tasks(&self, export: &MeetingExport, options: &RenderOptions) -> String {
        TasksMarkdownRenderer { export, options }.render()
    }

    /// `WebVTT` with one cue per segment and `<v Name>` voice spans.
    #[must_use]
    pub fn render_vtt(&self, export: &MeetingExport) -> String {
        WebVttRenderer { export }.render()
    }

    /// `meeting.json`, byte-identical to `steno export`. Serialised to
    /// text first so an `f32` keeps its shortest form.
    pub fn render_json(&self, export: &MeetingExport) -> Result<Vec<u8>, serde_json::Error> {
        let value: Value = serde_json::from_str(&serde_json::to_string(export)?)?;
        Ok(steno_core::json::to_canonical_string(&value)?.into_bytes())
    }

    /// The pinned folder basename, else this export's.
    fn slug(export: &MeetingExport, options: &RenderOptions, folder_slug: Option<&str>) -> String {
        folder_slug.map_or_else(
            || MeetingFolder::basename(&export.meeting, options.time_zone),
            str::to_owned,
        )
    }

    /// The lowercase UUID every note carries as `steno_id`.
    pub(crate) fn steno_id(export: &MeetingExport) -> String {
        export.meeting.id.to_string()
    }

    /// The frontmatter and `# Title — Kind` heading of the transcript and
    /// tasks notes.
    pub(crate) fn note_head(export: &MeetingExport, kind: &str, time_zone: Tz) -> Vec<String> {
        let mut frontmatter = Frontmatter::new(time_zone);
        frontmatter.append(
            "title",
            FrontmatterValue::String(format!("{} — {kind}", export.meeting.title)),
        );
        frontmatter.append("type", FrontmatterValue::String(kind.to_lowercase()));
        frontmatter.append("steno_id", FrontmatterValue::String(Self::steno_id(export)));
        vec![
            frontmatter.encoded(),
            format!(
                "# {} — {kind}\n",
                markdown_text::single_line(&export.meeting.title)
            ),
        ]
    }

    /// Segments by start, ties broken by id so the order is total.
    pub(crate) fn ordered_segments(export: &MeetingExport) -> Vec<&TranscriptSegment> {
        let mut segments: Vec<&TranscriptSegment> = export.segments.iter().collect();
        segments.sort_by(|left, right| {
            left.start
                .partial_cmp(&right.start)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.id.cmp(&right.id))
        });
        segments
    }
}
