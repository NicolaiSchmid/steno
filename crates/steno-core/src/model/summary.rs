//! The structured summary the LLM returns.
//! Swift: `Sources/StenoCore/Model/Summary.swift`.

use serde::{Deserialize, Serialize};

use super::LanguageTag;

/// The structured summary the LLM returns for one template. Stored as JSON
/// in `meeting.summary`; its `plain_text` fills `meeting.summaryText` for
/// full-text search.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryDocument {
    #[serde(rename = "templateID")]
    pub template_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<LanguageTag>,
    pub sections: Vec<SummarySection>,
}

impl SummaryDocument {
    /// Every bullet's lead and text, one line per bullet.
    #[must_use]
    pub fn plain_text(&self) -> String {
        self.sections
            .iter()
            .flat_map(|section| section.bullets.iter())
            .map(|bullet| {
                if bullet.lead.is_empty() {
                    bullet.text.clone()
                } else {
                    format!("{}: {}", bullet.lead, bullet.text)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// One template section with its bullets.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SummarySection {
    pub id: String,
    pub heading: String,
    pub bullets: Vec<SummaryBullet>,
}

/// A bullet in the `**lead**: text` shape.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SummaryBullet {
    pub lead: String,
    pub text: String,
}
