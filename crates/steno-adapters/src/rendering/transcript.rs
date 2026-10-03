//! The transcript as reading Markdown.
//! Swift: `Sources/StenoAdapters/Rendering/TranscriptMarkdownRenderer.swift`.

use steno_core::{MeetingExport, TranscriptSegment};
use uuid::Uuid;

use super::{ArtifactRenderer, Names, RenderOptions, Timecode, markdown_text};

/// One turn per run of consecutive segments by one speaker, a paragraph
/// break inside a turn at gaps of [`TranscriptMarkdownRenderer::PARAGRAPH_GAP`]
/// seconds or more.
pub(crate) struct TranscriptMarkdownRenderer<'a> {
    pub export: &'a MeetingExport,
    pub options: &'a RenderOptions,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Turn {
    pub speaker_id: Option<Uuid>,
    pub start: f64,
    pub paragraphs: Vec<String>,
}

impl TranscriptMarkdownRenderer<'_> {
    pub const PARAGRAPH_GAP: f64 = 3.0;

    pub fn render(&self) -> String {
        let names = Names {
            export: self.export,
            options: self.options,
        };
        let mut parts =
            ArtifactRenderer::note_head(self.export, "Transcript", self.options.time_zone);
        let turns = Self::turns(&ArtifactRenderer::ordered_segments(self.export));
        if turns.is_empty() {
            parts.push("No transcript.\n".to_owned());
        }
        for turn in turns {
            parts.push(format!(
                "## {} — {}\n",
                names.linked_speaker(turn.speaker_id),
                Timecode::clock(turn.start)
            ));
            let mut body = turn
                .paragraphs
                .iter()
                .map(|paragraph| markdown_text::escape_paragraph_start(paragraph))
                .collect::<Vec<_>>()
                .join("\n\n");
            body.push('\n');
            parts.push(body);
        }
        parts.join("\n")
    }

    /// Segments in start order grouped into turns; empty texts are skipped.
    pub fn turns(segments: &[&TranscriptSegment]) -> Vec<Turn> {
        let mut turns: Vec<Turn> = Vec::new();
        let mut previous_end = 0.0_f64;
        for segment in segments {
            let text = markdown_text::single_line(&segment.text);
            if text.is_empty() {
                continue;
            }
            match turns.last_mut() {
                Some(last) if last.speaker_id == segment.speaker_id => {
                    if segment.start - previous_end >= Self::PARAGRAPH_GAP {
                        last.paragraphs.push(text);
                    } else if let Some(paragraph) = last.paragraphs.last_mut() {
                        paragraph.push(' ');
                        paragraph.push_str(&text);
                    }
                }
                _ => turns.push(Turn {
                    speaker_id: segment.speaker_id,
                    start: segment.start,
                    paragraphs: vec![text],
                }),
            }
            previous_end = previous_end.max(segment.end);
        }
        turns
    }
}
