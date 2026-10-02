//! The summary templates: section ids and headings fixed by the core,
//! prompt text owned by the LLM workstream.
//! Swift: `Sources/StenoCore/Templates/SummaryTemplate.swift`.

use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

/// One summary template; the four bundled ones come from
/// `Sources/StenoCore/Resources/Templates/*.json`, the one copy both
/// implementations read until cutover.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryTemplate {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub context: String,
    pub sections: Vec<TemplateSection>,
}

/// One section the model fills.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TemplateSection {
    pub id: String,
    pub heading: String,
    pub instructions: String,
    pub required: bool,
}

/// The bundled template files in [`SummaryTemplate::BUNDLED_IDS`] order.
const BUNDLED_JSON: [&str; 4] = [
    include_str!("../../../../Sources/StenoCore/Resources/Templates/default.json"),
    include_str!("../../../../Sources/StenoCore/Resources/Templates/customer-discovery.json"),
    include_str!("../../../../Sources/StenoCore/Resources/Templates/daily-standup.json"),
    include_str!("../../../../Sources/StenoCore/Resources/Templates/interview.json"),
];

static BUNDLED: LazyLock<Vec<SummaryTemplate>> = LazyLock::new(|| {
    SummaryTemplate::BUNDLED_IDS
        .iter()
        .zip(BUNDLED_JSON)
        .map(|(id, json)| {
            let template: SummaryTemplate = serde_json::from_str(json).unwrap_or_else(|error| {
                panic!("summary template {id}.json failed to load: {error}")
            });
            assert_eq!(
                template.id, *id,
                "summary template {id}.json declares id {}",
                template.id
            );
            template
        })
        .collect()
});

impl SummaryTemplate {
    pub const DEFAULT_ID: &'static str = "default";

    /// The bundled template ids in menu order.
    pub const BUNDLED_IDS: [&'static str; 4] = [
        "default",
        "customer-discovery",
        "daily-standup",
        "interview",
    ];

    /// The four fixed templates in [`Self::BUNDLED_IDS`] order. Parsed once;
    /// a malformed file is a packaging error and panics with the file name.
    #[must_use]
    pub fn bundled() -> &'static [SummaryTemplate] {
        &BUNDLED
    }

    #[must_use]
    pub fn bundled_with_id(id: &str) -> Option<&'static SummaryTemplate> {
        Self::bundled().iter().find(|template| template.id == id)
    }

    #[must_use]
    pub fn section(&self, id: &str) -> Option<&TemplateSection> {
        self.sections.iter().find(|section| section.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_bundled_templates_load_in_menu_order() {
        let ids: Vec<&str> = SummaryTemplate::bundled()
            .iter()
            .map(|template| template.id.as_str())
            .collect();
        assert_eq!(ids, SummaryTemplate::BUNDLED_IDS);
        // The menu order `SummaryTemplate.bundledIDs` lists in
        // `Sources/StenoCore/Templates/SummaryTemplate.swift`.
        assert_eq!(
            SummaryTemplate::BUNDLED_IDS,
            [
                "default",
                "customer-discovery",
                "daily-standup",
                "interview"
            ]
        );
        let default = SummaryTemplate::bundled_with_id(SummaryTemplate::DEFAULT_ID).unwrap();
        assert_eq!(default.display_name, "Default");
        assert!(default.section("executive-summary").unwrap().required);
        assert!(SummaryTemplate::bundled_with_id("missing").is_none());
    }
}
