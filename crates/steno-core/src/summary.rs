//! The summary as Markdown, at display and export time.
//! Swift: `Sources/StenoCore/Summary/SummaryMarkdown.swift`.
//!
//! `## heading` per section, `- **lead**: text` per bullet, and every
//! speaker cluster label ("Speaker 2") replaced by the speaker's current
//! name in bold. Renaming a speaker is therefore a re-render, never an LLM
//! re-run. [`sections`] is the structured form [`render`] joins; a UI that
//! shows sections renders those and never parses the Markdown back.
//! Adapters that need a heading offset shift the `##` themselves.

use crate::model::MeetingExport;

/// One summary section as rendered: the heading and its bullets as inline
/// Markdown (`**lead**: text`) with every speaker cluster label already
/// replaced by the speaker's current name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RenderedSection {
    /// The `SummarySection.id` (a template section id).
    pub id: String,
    pub heading: String,
    /// Inline Markdown per bullet, without the list marker.
    pub bullets: Vec<String>,
}

impl RenderedSection {
    /// The bullets as a Markdown list, one `- ` line each, no trailing
    /// newline.
    #[must_use]
    pub fn body(&self) -> String {
        self.bullets
            .iter()
            .map(|bullet| format!("- {bullet}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// `## heading`, a blank line, then [`RenderedSection::body`].
    #[must_use]
    pub fn markdown(&self) -> String {
        format!("## {}\n\n{}", self.heading, self.body())
    }
}

/// Every section with at least one bullet, in document order, names
/// substituted. Empty when the meeting has no summary yet.
#[must_use]
pub fn sections(export: &MeetingExport) -> Vec<RenderedSection> {
    let Some(summary) = &export.meeting.summary else {
        return Vec::new();
    };
    let names = speaker_names(export);
    summary
        .sections
        .iter()
        .filter(|section| !section.bullets.is_empty())
        .map(|section| RenderedSection {
            id: section.id.clone(),
            heading: section.heading.clone(),
            bullets: section
                .bullets
                .iter()
                .map(|bullet| {
                    let lead = substitute(&bullet.lead, &names, false);
                    let text = substitute(&bullet.text, &names, true);
                    if lead.is_empty() {
                        text
                    } else {
                        format!("**{lead}**: {text}")
                    }
                })
                .collect(),
        })
        .collect()
}

/// [`sections`] joined by blank lines, with one trailing newline; the empty
/// string when there is nothing to render.
#[must_use]
pub fn render(export: &MeetingExport) -> String {
    let sections = sections(export);
    if sections.is_empty() {
        return String::new();
    }
    let mut text = sections
        .iter()
        .map(RenderedSection::markdown)
        .collect::<Vec<_>>()
        .join("\n\n");
    text.push('\n');
    text
}

/// Cluster label to current display name, for speakers that resolved to a
/// person; unresolved labels stay as they are. Longest labels first so
/// "Speaker 10" is never matched by "Speaker 1".
fn speaker_names(export: &MeetingExport) -> Vec<(String, String)> {
    let mut names: Vec<(String, String)> = export
        .speakers
        .iter()
        .filter(|speaker| speaker.person_id().is_some())
        .filter_map(|speaker| {
            let name = export.display_name_for_speaker(speaker.id);
            (!name.is_empty() && name != speaker.cluster_label)
                .then(|| (speaker.cluster_label.clone(), name))
        })
        .collect();
    names.sort_by(|left, right| {
        right
            .0
            .chars()
            .count()
            .cmp(&left.0.chars().count())
            .then_with(|| left.0.cmp(&right.0))
    });
    names
}

/// Replaces every label that stands as a whole word (no letter or digit
/// touching it on either side), in one pass over `text`, so "Me" never
/// matches inside "Meeting" and a name written in never gets matched again.
fn substitute(text: &str, names: &[(String, String)], bold: bool) -> String {
    if names.is_empty() {
        return text.to_owned();
    }
    let mut result = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        let rest = &text[index..];
        let matched = is_boundary_before(text, index)
            .then(|| {
                names.iter().find(|(label, _)| {
                    rest.starts_with(label.as_str()) && is_boundary_after(text, index + label.len())
                })
            })
            .flatten();
        if let Some((label, name)) = matched {
            if bold {
                result.push_str("**");
                result.push_str(name);
                result.push_str("**");
            } else {
                result.push_str(name);
            }
            index += label.len();
        } else {
            let character = rest.chars().next().expect("index is on a char boundary");
            result.push(character);
            index += character.len_utf8();
        }
    }
    result
}

fn is_boundary_before(text: &str, index: usize) -> bool {
    text[..index]
        .chars()
        .next_back()
        .is_none_or(|character| !character.is_alphanumeric())
}

fn is_boundary_after(text: &str, index: usize) -> bool {
    text[index..]
        .chars()
        .next()
        .is_none_or(|character| !character.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Meeting, MeetingSource, MeetingState, Person, Speaker, SpeakerAssignment, SummaryBullet,
        SummaryDocument, SummarySection, TitleOrigin,
    };
    use chrono::{TimeZone as _, Utc};
    use uuid::Uuid;

    fn export() -> MeetingExport {
        let meeting_id = Uuid::from_u128(1);
        let anna = Uuid::from_u128(10);
        let now = Utc.with_ymd_and_hms(2026, 9, 24, 12, 0, 0).unwrap();
        let speaker = |n: u128, label: &str, person: Option<Uuid>| Speaker {
            id: Uuid::from_u128(n),
            meeting_id,
            cluster_label: label.to_owned(),
            assignment: person.map_or(SpeakerAssignment::Unknown, |person_id| {
                SpeakerAssignment::Confirmed { person_id }
            }),
            embedding: None,
            sample_clip_range: None,
            sample_clip_url: None,
            cluster_confidence: 1.0,
        };
        MeetingExport {
            schema_version: 1,
            meeting: Meeting {
                id: meeting_id,
                title: "Sync".to_owned(),
                started_at: now,
                duration: 60.0,
                language: None,
                source: MeetingSource::MacCall,
                calendar_event_id: None,
                tags: vec![],
                state: MeetingState::Ready,
                end_reason: None,
                title_origin: TitleOrigin::Default,
                template_id: "default".to_owned(),
                summary: Some(SummaryDocument {
                    template_id: "default".to_owned(),
                    language: None,
                    sections: vec![
                        SummarySection {
                            id: "a".to_owned(),
                            heading: "Überblick".to_owned(),
                            bullets: vec![
                                SummaryBullet {
                                    lead: "Speaker 1".to_owned(),
                                    text: "Speaker 10 and Speaker 1 met; Speaker 1s stay."
                                        .to_owned(),
                                },
                                SummaryBullet {
                                    lead: String::new(),
                                    text: "Me in Meeting is not Me.".to_owned(),
                                },
                            ],
                        },
                        SummarySection {
                            id: "b".to_owned(),
                            heading: "Leer".to_owned(),
                            bullets: vec![],
                        },
                    ],
                }),
                scratchpad: String::new(),
                llm_usage: None,
                created_at: now,
                updated_at: now,
            },
            participants: vec![],
            speakers: vec![
                speaker(20, "Speaker 1", Some(anna)),
                speaker(21, "Speaker 10", None),
                speaker(22, "Me", Some(anna)),
            ],
            persons: vec![Person {
                id: anna,
                display_name: "Anna".to_owned(),
                email: None,
                embedding: None,
                sample_count: 1,
                created_at: now,
            }],
            segments: vec![],
            tasks: vec![],
            decisions: vec![],
            audio: None,
        }
    }

    #[test]
    fn labels_are_replaced_as_whole_words_longest_first_and_empty_sections_dropped() {
        let sections = sections(&export());
        assert_eq!(sections.len(), 1, "a section without bullets is dropped");
        assert_eq!(sections[0].heading, "Überblick");
        assert_eq!(
            sections[0].bullets,
            [
                "**Anna**: Speaker 10 and **Anna** met; Speaker 1s stay.",
                "**Anna** in Meeting is not **Anna**.",
            ]
        );
        assert_eq!(
            render(&export()),
            "## Überblick\n\n- **Anna**: Speaker 10 and **Anna** met; Speaker 1s stay.\n- **Anna** in Meeting is not **Anna**.\n"
        );
    }

    #[test]
    fn no_summary_renders_nothing() {
        let mut export = export();
        export.meeting.summary = None;
        assert_eq!(sections(&export), vec![]);
        assert_eq!(render(&export), "");
    }
}
