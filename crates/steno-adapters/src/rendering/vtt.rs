//! `WebVTT` per the W3C spec.
//! Swift: `Sources/StenoAdapters/Rendering/WebVTTRenderer.swift`.

use steno_core::MeetingExport;

use super::{ArtifactRenderer, Names, RenderOptions, Timecode, date_text, markdown_text};

/// `hh:mm:ss.ttt` timestamps, one cue per segment in start order,
/// `<v Name>text` voice spans, `NOTE` block with title and date. Payload
/// text escapes `& < >` and drops line breaks and `-->`; voice annotations
/// drop `& >` and line breaks.
pub(crate) struct WebVttRenderer<'a> {
    pub export: &'a MeetingExport,
}

impl WebVttRenderer<'_> {
    pub const MINIMUM_CUE: f64 = 0.001;

    pub fn render(&self) -> String {
        let names = Names {
            export: self.export,
            options: &RenderOptions::PLAIN,
        };
        let mut blocks = vec![format!(
            "WEBVTT - Steno {}",
            ArtifactRenderer::steno_id(self.export)
        )];
        blocks.push(format!(
            "NOTE\n{}\n{}",
            Self::note_text(&self.export.meeting.title),
            date_text::utc(self.export.meeting.started_at)
        ));
        for segment in ArtifactRenderer::ordered_segments(self.export) {
            let start = segment.start.max(0.0);
            let end = segment.end.max(start + Self::MINIMUM_CUE);
            blocks.push(format!(
                "{} --> {}\n<v {}>{}",
                Timecode::vtt(start),
                Timecode::vtt(end),
                Self::annotation(&names.speaker(segment.speaker_id)),
                Self::payload(&segment.text)
            ));
        }
        let mut text = blocks.join("\n\n");
        text.push('\n');
        text
    }

    pub fn payload(text: &str) -> String {
        markdown_text::single_line(text)
            .replace("-->", "")
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    pub fn annotation(name: &str) -> String {
        markdown_text::single_line(name).replace(['&', '>'], "")
    }

    pub fn note_text(text: &str) -> String {
        let single = markdown_text::single_line(text).replace("-->", "");
        if single.is_empty() {
            "Untitled".to_owned()
        } else {
            single
        }
    }
}
