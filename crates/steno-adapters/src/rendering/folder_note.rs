//! The meeting's folder note: the file Obsidian opens when the folder is
//! clicked.
//! Swift: `Sources/StenoAdapters/Rendering/FolderNoteRenderer.swift`.

use steno_core::{MeetingExport, MeetingSource, Platform, summary};

use super::{
    ArtifactRenderer, Frontmatter, FrontmatterValue, LinkStyle, Names, RenderOptions, date_text,
    markdown_text,
};
use crate::naming::{MeetingFolder, Note};

pub(crate) struct FolderNoteRenderer<'a> {
    pub export: &'a MeetingExport,
    pub options: &'a RenderOptions,
    pub folder_slug: &'a str,
}

impl FolderNoteRenderer<'_> {
    pub fn render(&self) -> String {
        let meeting = &self.export.meeting;
        let mut parts = vec![self.frontmatter().encoded()];
        parts.push(format!(
            "# {}\n",
            markdown_text::single_line(&meeting.title)
        ));
        parts.push(format!("{}\n", self.info_line()));
        parts.push("## Summary\n".to_owned());
        let rendered = Self::demoted(&summary::render(self.export));
        parts.push(if rendered.is_empty() {
            "No summary.\n".to_owned()
        } else {
            rendered
        });
        if !self.export.decisions.is_empty() {
            parts.push("## Decisions\n".to_owned());
            let mut decisions = self
                .export
                .decisions
                .iter()
                .map(|decision| format!("- {}", markdown_text::single_line(&decision.text)))
                .collect::<Vec<_>>()
                .join("\n");
            decisions.push('\n');
            parts.push(decisions);
        }
        let scratchpad = meeting.scratchpad.trim();
        if !scratchpad.is_empty() {
            parts.push("## Scratchpad\n".to_owned());
            parts.push(format!("{scratchpad}\n"));
        }
        parts.join("\n")
    }

    pub fn frontmatter(&self) -> Frontmatter {
        let meeting = &self.export.meeting;
        let mut frontmatter = Frontmatter::new(self.options.time_zone);
        frontmatter.append("title", FrontmatterValue::String(meeting.title.clone()));
        frontmatter.append("date", FrontmatterValue::DateTime(meeting.started_at));
        frontmatter.append(
            "duration",
            FrontmatterValue::Int(Self::whole_minutes(meeting.duration)),
        );
        frontmatter.append(
            "participants",
            FrontmatterValue::List(
                Names {
                    export: self.export,
                    options: self.options,
                }
                .participants(),
            ),
        );
        // Sanitised to Obsidian's tag grammar here, quoted by the emitter so a
        // tag of `2026` or `true` stays a string.
        let tags = std::iter::once("meeting")
            .chain(meeting.tags.iter().map(String::as_str))
            .filter_map(markdown_text::tag)
            .collect();
        frontmatter.append("tags", FrontmatterValue::List(tags));
        frontmatter.append(
            "source",
            FrontmatterValue::String(Self::source_key(meeting.source).to_owned()),
        );
        frontmatter.append(
            "template",
            FrontmatterValue::String(meeting.template_id.clone()),
        );
        if let Some(language) = &meeting.language {
            frontmatter.append(
                "language",
                FrontmatterValue::String(language.as_str().to_owned()),
            );
        }
        frontmatter.append(
            "steno_id",
            FrontmatterValue::String(ArtifactRenderer::steno_id(self.export)),
        );
        frontmatter
    }

    /// `2026-09-24 14:00–15:30 · 1 h 30 min · Mac call · [[slug - Transcript|Transcript]] · [[slug - Tasks|Tasks]]`,
    /// with "Windows call" or "Linux call" for a call the options place there.
    pub fn info_line(&self) -> String {
        let meeting = &self.export.meeting;
        let start = meeting.started_at;
        // The raw seconds, as Swift's `addingTimeInterval(max(0, duration))`:
        // rounding to milliseconds first would move an end a hair under a
        // minute boundary onto it. Negative, NaN and absurd durations add
        // nothing.
        let end = start
            + std::time::Duration::try_from_secs_f64(meeting.duration)
                .ok()
                .and_then(|duration| chrono::Duration::from_std(duration).ok())
                .unwrap_or_default();
        let links = match self.options.link_style {
            LinkStyle::Wikilink => [
                markdown_text::wikilink(
                    &MeetingFolder::note_name(Note::Transcript, self.folder_slug),
                    Some("Transcript"),
                ),
                markdown_text::wikilink(
                    &MeetingFolder::note_name(Note::Tasks, self.folder_slug),
                    Some("Tasks"),
                ),
            ],
            LinkStyle::None => [
                markdown_text::markdown_link(
                    "Transcript",
                    &MeetingFolder::note_file(Note::Transcript, self.folder_slug),
                ),
                markdown_text::markdown_link(
                    "Tasks",
                    &MeetingFolder::note_file(Note::Tasks, self.folder_slug),
                ),
            ],
        };
        let zone = self.options.time_zone;
        let when = format!(
            "{} {}–{}",
            date_text::day(start, zone),
            date_text::clock(start, zone),
            date_text::clock(end, zone)
        );
        [
            when,
            Self::duration_text(meeting.duration),
            Self::source_label(meeting.source, self.options.platform).to_owned(),
        ]
        .into_iter()
        .chain(links)
        .collect::<Vec<_>>()
        .join(" · ")
    }

    /// Core renders sections as `## heading`; under `## Summary` they become
    /// `### heading`, the offset `summary::render` leaves to adapters.
    pub fn demoted(summary: &str) -> String {
        summary
            .split('\n')
            .map(|line| {
                if line.starts_with('#') {
                    format!("#{line}")
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Whole minutes, rounded up.
    pub fn whole_minutes(duration: f64) -> i64 {
        if !duration.is_finite() || duration <= 0.0 {
            return 0;
        }
        // Finite, positive and already rounded: exact below 2^53.
        #[allow(clippy::cast_possible_truncation)]
        let minutes = (duration / 60.0).ceil() as i64;
        minutes
    }

    /// `1 h 30 min`, `2 h`, `45 min`.
    pub fn duration_text(duration: f64) -> String {
        let minutes = Self::whole_minutes(duration);
        let hours = minutes / 60;
        let rest = minutes % 60;
        match (hours, rest) {
            (0, _) => format!("{rest} min"),
            (_, 0) => format!("{hours} h"),
            _ => format!("{hours} h {rest} min"),
        }
    }

    /// The frontmatter's `source`: a stable data key that vault queries
    /// match on, so a call is `mac-call` whichever platform recorded it;
    /// only the info line's [`source_label`](Self::source_label) names the
    /// platform.
    pub fn source_key(source: MeetingSource) -> &'static str {
        match source {
            MeetingSource::MacCall => "mac-call",
            MeetingSource::MacInPerson => "mac-in-person",
            MeetingSource::Phone => "phone",
        }
    }

    /// The info line's word for the source; a call names the platform it
    /// was recorded on. Swift: `sourceLabel`, always the Mac's.
    pub fn source_label(source: MeetingSource, platform: Platform) -> &'static str {
        match source {
            MeetingSource::MacCall => match platform {
                Platform::Macos => "Mac call",
                Platform::Windows => "Windows call",
                Platform::Linux => "Linux call",
            },
            MeetingSource::MacInPerson => "In person",
            MeetingSource::Phone => "Phone",
        }
    }
}
