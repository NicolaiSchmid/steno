//! The summary as the detail pane lays it out, after
//! `Sources/StenoCore/Summary/SummaryMarkdown.swift` (the sections with
//! every speaker cluster label replaced by the speaker's current name) and
//! the bullet split of `MainWindowSnapshots.bullet`. Renaming a speaker is
//! therefore a re-render, never an LLM re-run. Moves to the core with the
//! export renderer (WP6's `export`); the host imports it then.

use steno_bridge::{DetailSummaryBullet, DetailSummarySection};
use steno_core::MeetingExport;

/// One summary section as rendered: the heading and its bullets as inline
/// Markdown (`**lead**: text`), names substituted. Swift: `RenderedSection`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedSection {
    pub id: String,
    pub heading: String,
    pub bullets: Vec<String>,
}

/// Every section with at least one bullet, in document order, names
/// substituted. Empty when the meeting has no summary yet.
/// Swift: `SummaryMarkdown.sections(for:)`.
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

/// The sections in the wire vocabulary: each rendered bullet split back
/// into its lead and text, the bold markers dropped, as the page shows
/// plain sentences. Swift: `MeetingDetailSnapshot.init` over
/// `MainWindowSnapshots.bullet`.
#[must_use]
pub fn detail_sections(export: &MeetingExport) -> Vec<DetailSummarySection> {
    sections(export)
        .into_iter()
        .map(|section| DetailSummarySection {
            id: section.id,
            heading: section.heading,
            bullets: section.bullets.iter().map(|text| bullet(text)).collect(),
        })
        .collect()
}

/// A rendered bullet (`**lead**: text`) back into its lead and text.
/// Swift: `MainWindowSnapshots.bullet`.
#[must_use]
pub fn bullet(rendered: &str) -> DetailSummaryBullet {
    if let Some(after_marker) = rendered.strip_prefix("**")
        && let Some(close) = after_marker.find("**: ")
    {
        return DetailSummaryBullet {
            lead: after_marker[..close].to_owned(),
            text: plain(&after_marker[close + 4..]),
        };
    }
    DetailSummaryBullet {
        lead: String::new(),
        text: plain(rendered),
    }
}

/// Inline Markdown without its bold markers. Swift: `MainWindowSnapshots.plain`.
#[must_use]
pub fn plain(inline: &str) -> String {
    inline.replace("**", "")
}

/// Cluster label to current display name, for speakers that resolved to a
/// person; unresolved labels stay. Longest labels first so "Speaker 10" is
/// never matched by "Speaker 1". Swift: `SummaryMarkdown.speakerNames`.
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
/// Swift: `SummaryMarkdown.substitute`.
fn substitute(text: &str, names: &[(String, String)], bold: bool) -> String {
    if names.is_empty() {
        return text.to_owned();
    }
    let mut result = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        let rest = &text[index..];
        let matched = boundary_before(text, index)
            .then(|| {
                names.iter().find(|(label, _)| {
                    rest.starts_with(label.as_str()) && boundary_after(text, index + label.len())
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

fn boundary_before(text: &str, index: usize) -> bool {
    text[..index]
        .chars()
        .next_back()
        .is_none_or(|character| !character.is_alphanumeric())
}

fn boundary_after(text: &str, index: usize) -> bool {
    text[index..]
        .chars()
        .next()
        .is_none_or(|character| !character.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bullets_split_the_lead_from_the_text_and_drop_bold_markers() {
        assert_eq!(
            bullet("**Fokus**: Wir setzen auf **Nicolai**."),
            DetailSummaryBullet {
                lead: "Fokus".to_owned(),
                text: "Wir setzen auf Nicolai.".to_owned()
            }
        );
        assert_eq!(
            bullet("Nur Text mit **Jérôme**"),
            DetailSummaryBullet {
                lead: String::new(),
                text: "Nur Text mit Jérôme".to_owned()
            }
        );
        assert_eq!(bullet("**unterminated").lead, "");
    }

    #[test]
    fn substitution_respects_word_boundaries_and_longest_labels() {
        let names = vec![
            ("Speaker 10".to_owned(), "Anna".to_owned()),
            ("Speaker 1".to_owned(), "Nicolai".to_owned()),
        ];
        assert_eq!(
            substitute("Speaker 1 und Speaker 10 und Speaker 1x", &names, true),
            "**Nicolai** und **Anna** und Speaker 1x"
        );
        assert_eq!(substitute("Speaker 1", &names, false), "Nicolai");
        assert_eq!(substitute("Me", &[], true), "Me");
    }
}
