//! The summary as the detail pane lays it out: the core's rendered
//! sections (`steno_core::summary`, cluster labels replaced by current
//! names) with each bullet split back into its lead and text.

use steno_bridge::{DetailSummaryBullet, DetailSummarySection};
use steno_core::MeetingExport;
use steno_core::summary::sections;

/// The sections in the wire vocabulary: each bullet split into its lead
/// and text, the bold markers dropped.
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

/// Inline Markdown without its bold markers.
#[must_use]
pub fn plain(inline: &str) -> String {
    inline.replace("**", "")
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
}
