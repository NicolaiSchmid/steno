//! A YAML frontmatter block built from typed values.
//! Swift: `Sources/StenoAdapters/Rendering/Frontmatter.swift`.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;

use super::Timecode;
use super::date_text;

/// One typed frontmatter value.
#[derive(Debug, Clone, PartialEq)]
pub enum FrontmatterValue {
    String(String),
    Int(i64),
    Bool(bool),
    /// `2026-09-24`, in the frontmatter's time zone.
    Date(DateTime<Utc>),
    /// `2026-09-24T14:00:00`, in the frontmatter's time zone, no offset.
    DateTime(DateTime<Utc>),
    /// A list of quoted strings.
    List(Vec<String>),
}

/// A YAML frontmatter block built from typed values, never from string
/// interpolation. Every string is double-quoted with `\\`, `\"` and control
/// characters escaped, so titles with colons, quotes, `#` or a leading `-`
/// are safe and a tag of `2026`, `true` or `null` stays a string; dates,
/// numbers and booleans are the plain scalars Obsidian types as Date, Date &
/// time, Number and Checkbox. The emitter knows no consumer: tag grammar
/// belongs to the renderer that builds the list.
#[derive(Debug, Clone, PartialEq)]
pub struct Frontmatter {
    /// Insertion order is output order.
    pub fields: Vec<(String, FrontmatterValue)>,
    pub time_zone: Tz,
}

impl Frontmatter {
    #[must_use]
    pub fn new(time_zone: Tz) -> Self {
        Frontmatter {
            fields: Vec::new(),
            time_zone,
        }
    }

    pub fn append(&mut self, key: &str, value: FrontmatterValue) {
        self.fields.push((key.to_owned(), value));
    }

    /// `"---\n…\n---\n"`.
    #[must_use]
    pub fn encoded(&self) -> String {
        let mut lines = vec!["---".to_owned()];
        for (key, value) in &self.fields {
            match value {
                FrontmatterValue::String(string) => {
                    lines.push(format!("{key}: {}", Self::quoted(string)));
                }
                FrontmatterValue::Int(int) => lines.push(format!("{key}: {int}")),
                FrontmatterValue::Bool(bool) => lines.push(format!("{key}: {bool}")),
                FrontmatterValue::Date(date) => {
                    lines.push(format!("{key}: {}", date_text::day(*date, self.time_zone)));
                }
                FrontmatterValue::DateTime(date) => lines.push(format!(
                    "{key}: {}",
                    date_text::date_time(*date, self.time_zone)
                )),
                FrontmatterValue::List(items) => {
                    if items.is_empty() {
                        lines.push(format!("{key}: []"));
                    } else {
                        lines.push(format!("{key}:"));
                        lines.extend(
                            items
                                .iter()
                                .map(|item| format!("  - {}", Self::quoted(item))),
                        );
                    }
                }
            }
        }
        lines.push("---".to_owned());
        let mut text = lines.join("\n");
        text.push('\n');
        text
    }

    /// A YAML double-quoted scalar: `\` and `"` escaped; C0 and C1 controls,
    /// DEL, the line and paragraph separators and the byte order mark as
    /// `\n`, `\t`, `\r` or `\uXXXX` (YAML forbids them unescaped); everything
    /// else, including non-ASCII, verbatim.
    #[must_use]
    pub fn quoted(string: &str) -> String {
        let mut result = String::from("\"");
        for character in string.chars() {
            match character {
                '\\' => result.push_str("\\\\"),
                '"' => result.push_str("\\\""),
                '\n' => result.push_str("\\n"),
                '\t' => result.push_str("\\t"),
                '\r' => result.push_str("\\r"),
                other if Self::needs_escape(other) => {
                    result.push_str("\\u");
                    result.push_str(&Timecode::pad_hex(u32::from(other), 4));
                }
                other => result.push(other),
            }
        }
        result.push('"');
        result
    }

    fn needs_escape(character: char) -> bool {
        matches!(
            u32::from(character),
            0x00..0x20 | 0x7F..=0x9F | 0x2028 | 0x2029 | 0xFEFF
        )
    }
}
