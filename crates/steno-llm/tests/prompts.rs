//! Golden prompts for every builder. Fixed meeting date, UTC, English
//! language names, so the files are identical on every machine and equal
//! to what the Swift builders write. A diff here is a reviewed prompt
//! change and needs a sentence in the PR.
//! Swift: `PromptSnapshotTests`, `TemplateBlocksTests`, `OutputLanguageTests`.

mod common;

use chrono::Utc;
use common::*;
use steno_core::{LanguageTag, SummaryTemplate};
use steno_llm::cleanup::glossary;
use steno_llm::inputs::{cleanup_input, summary_input};
use steno_llm::labels::SpeakerLabels;
use steno_llm::language::OutputLanguage;
use steno_llm::{CleanupPromptBuilder, SummaryPromptBuilder, TranscriptChunker};

fn cleanup_request(language: Option<&LanguageTag>) -> steno_core::LlmRequest {
    let input = cleanup_input(&standup());
    let chunks = TranscriptChunker::new(150, 220, 3).chunk(&input.segments, language);
    CleanupPromptBuilder::default().build(
        &chunks[1],
        language,
        &glossary(&input),
        &SpeakerLabels::new(&input.speakers),
    )
}

#[test]
fn cleanup_prompt_german() {
    let request = cleanup_request(Some(&LanguageTag::from("de")));
    assert_golden(&render(&request), "prompts/cleanup-de.txt");
    let user = &request.messages[1].content;
    assert!(user.contains("Context (read-only, do not return):"));
    assert!(user.contains("[context] Speaker"));
    assert!(user.contains("[0] Speaker"));
}

#[test]
fn cleanup_prompt_english() {
    let request = cleanup_request(Some(&LanguageTag::from("en")));
    assert_golden(&render(&request), "prompts/cleanup-en.txt");
    assert!(
        request.messages[0]
            .content
            .contains("Meeting language: English.")
    );
}

#[test]
fn single_shot_prompts_match_their_goldens() {
    let standup = standup();
    for id in SummaryTemplate::BUNDLED_IDS {
        let input = summary_input(&standup, SummaryTemplate::bundled_with_id(id));
        let request =
            SummaryPromptBuilder::new(input.template.clone(), Utc).build_single_shot(&input);
        assert_golden(
            &render(&request),
            &format!("prompts/summary-single-{id}.txt"),
        );
        let system = &request.messages[0].content;
        assert!(system.contains("Output language: German."));
        assert!(system.contains("Meeting date: 2026-09-24 (Thursday)."));
        assert!(system.contains(
            "- Speaker 2 (unknown; suggest a name only with evidence from the transcript)"
        ));
        assert!(system.contains("- Speaker 3 = Jérôme"));
        assert!(system.contains("- Speaker 1 = probably Mara (unconfirmed)"));
        assert!(system.contains("- Nicolai (the user, \"me\")"));
        assert!(
            request.messages[1]
                .content
                .starts_with("Transcript:\nSpeaker 1: okay lass uns anfangen.")
        );
    }
}

#[test]
fn an_untagged_meeting_is_summarised_in_english() {
    let mut export = standup();
    export.meeting.language = None;
    let input = summary_input(&export, SummaryTemplate::bundled_with_id("default"));
    let request = SummaryPromptBuilder::new(input.template.clone(), Utc).build_single_shot(&input);
    assert_golden(&render(&request), "prompts/summary-single-default-en.txt");
    assert!(
        request.messages[0]
            .content
            .contains("Output language: English.")
    );
}

#[test]
fn template_blocks_match_their_goldens() {
    for id in SummaryTemplate::BUNDLED_IDS {
        let template = SummaryTemplate::bundled_with_id(id).unwrap();
        let blocks = SummaryPromptBuilder::new(template.clone(), Utc).template_blocks();
        assert_golden(
            &format!("{blocks}\n"),
            &format!("prompts/template-blocks-{id}.txt"),
        );
        assert!(blocks.contains(&format!("Template: {}", template.display_name)));
        assert!(blocks.contains(&template.context));
        for section in &template.sections {
            assert!(blocks.contains(&format!("- {} — \"{}\"", section.id, section.heading)));
            assert!(blocks.contains(&section.instructions));
            assert!(!section.instructions.ends_with(' '));
            assert!(section.instructions.ends_with('.'), "{id}/{}", section.id);
        }
        assert!(blocks.contains("(required)"));
    }
}

#[test]
fn section_ids_are_still_cores_and_only_wording_changed() {
    let ids: Vec<&str> = SummaryTemplate::bundled_with_id("default")
        .unwrap()
        .sections
        .iter()
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(ids, ["executive-summary", "full-summary", "open-questions"]);
    let headings: Vec<&str> = SummaryTemplate::bundled_with_id("daily-standup")
        .unwrap()
        .sections
        .iter()
        .map(|s| s.heading.as_str())
        .collect();
    assert_eq!(
        headings,
        [
            "Progress Since Last Standup",
            "Plans Until Next Standup",
            "Blockers and Help Needed",
            "Announcements"
        ]
    );
    for template in SummaryTemplate::bundled() {
        let context = template.context.to_lowercase();
        assert!(
            context.contains("never invent") || context.contains("never merge"),
            "{}",
            template.id
        );
    }
}

#[test]
fn output_language_resolves_and_names_in_english() {
    let tag = |t: &str| LanguageTag::from(t);
    assert_eq!(OutputLanguage::resolve(Some(&tag("de"))), tag("de"));
    assert_eq!(OutputLanguage::resolve(Some(&tag("de-CH"))), tag("de-CH"));
    assert_eq!(OutputLanguage::resolve(None), tag("en"));
    assert_eq!(OutputLanguage::resolve(Some(&tag(""))), tag("en"));
    assert_eq!(OutputLanguage::prompt_name(&tag("de")), "German");
    assert_eq!(OutputLanguage::prompt_name(&tag("de-AT")), "German");
    assert_eq!(OutputLanguage::prompt_name(&tag("en-US")), "English");
    assert_eq!(OutputLanguage::prompt_name(&tag("fr")), "French");
    assert_eq!(OutputLanguage::prompt_name(&tag("zz-Zz")), "zz-Zz");
}
