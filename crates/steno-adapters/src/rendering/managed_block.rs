//! The region of a person page that Steno owns, as text; the Obsidian
//! destination writes it.
//! Swift: `Sources/StenoAdapters/Obsidian/ManagedBlock.swift`.

use uuid::Uuid;

/// One line per meeting between two HTML comments, newest first, each line
/// ending in a `%%steno:<uuid>%%` comment that identifies its meeting.
/// [`ManagedBlock::merge`] replaces or inserts a meeting's line,
/// [`ManagedBlock::remove`] drops it again when the person left the meeting
/// (a speaker reassigned to somebody else). Bytes outside the markers are
/// copied unchanged; missing markers are appended by `merge` and never by
/// `remove`.
pub struct ManagedBlock;

impl ManagedBlock {
    pub const START: &'static str = "<!-- steno:meetings:start -->";
    pub const END: &'static str = "<!-- steno:meetings:end -->";

    /// `%%steno:0d6f…%%`, Obsidian's comment syntax so the id never shows;
    /// the id in `Uuid`'s lowercase `Display` form.
    #[must_use]
    pub fn marker(meeting_id: Uuid) -> String {
        format!("%%steno:{meeting_id}%%")
    }

    /// A whole block around `lines`, terminated by a newline.
    #[must_use]
    pub fn block(lines: &[String]) -> String {
        let mut text = String::from(Self::START);
        for line in lines {
            text.push('\n');
            text.push_str(line);
        }
        text.push('\n');
        text.push_str(Self::END);
        text.push('\n');
        text
    }

    /// The byte range of the block body: after the start marker, before the
    /// end marker that follows it.
    fn body_range(existing: &str) -> Option<(usize, usize)> {
        let start = existing.find(Self::START)? + Self::START.len();
        let end = start + existing[start..].find(Self::END)?;
        Some((start, end))
    }

    /// `existing` with `line` replacing the line that carries this meeting's
    /// marker (or inserted when there is none), the block re-sorted newest
    /// first; the block appended when the markers are missing.
    #[must_use]
    pub fn merge(line: &str, meeting_id: Uuid, existing: &str) -> String {
        let Some((start, end)) = Self::body_range(existing) else {
            let mut result = existing.to_owned();
            if !result.is_empty() && !result.ends_with('\n') {
                result.push('\n');
            }
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(&Self::block(&[line.to_owned()]));
            return result;
        };
        let marker = Self::marker(meeting_id);
        let mut lines: Vec<String> = existing[start..end]
            .split('\n')
            .filter(|candidate| !candidate.trim_matches([' ', '\t']).is_empty())
            .filter(|candidate| !candidate.contains(&marker))
            .map(str::to_owned)
            .collect();
        lines.push(line.to_owned());
        format!(
            "{}\n{}\n{}",
            &existing[..start],
            Self::sorted_newest_first(lines).join("\n"),
            &existing[end..]
        )
    }

    /// `existing` without the line that carries this meeting's marker. Every
    /// other line of the block keeps its bytes and order, an emptied block
    /// keeps its markers, and `existing` comes back unchanged when it has no
    /// block or the block has no line for this meeting.
    #[must_use]
    pub fn remove(meeting_id: Uuid, existing: &str) -> String {
        let Some((start, end)) = Self::body_range(existing) else {
            return existing.to_owned();
        };
        let marker = Self::marker(meeting_id);
        let body = &existing[start..end];
        if !body.contains(&marker) {
            return existing.to_owned();
        }
        let kept = body
            .split('\n')
            .filter(|candidate| !candidate.contains(&marker))
            .collect::<Vec<_>>()
            .join("\n");
        format!("{}{kept}{}", &existing[..start], &existing[end..])
    }

    /// By the leading `- YYYY-MM-DD` descending, then by text so equal dates
    /// keep a stable order; lines without a date sort last.
    #[must_use]
    pub fn sorted_newest_first(mut lines: Vec<String>) -> Vec<String> {
        lines.sort_by(|left, right| {
            Self::date_of(right)
                .cmp(&Self::date_of(left))
                .then_with(|| left.cmp(right))
        });
        lines
    }

    /// `"2026-09-24"` from `- 2026-09-24 …`, or `""` when the line has none.
    fn date_of(line: &str) -> String {
        let candidate: String = line
            .trim_start_matches(['-', ' ', '*'])
            .chars()
            .take(10)
            .collect();
        let bytes = candidate.as_bytes();
        if bytes.len() != 10 {
            return String::new();
        }
        let well_formed = bytes.iter().enumerate().all(|(index, byte)| {
            if index == 4 || index == 7 {
                *byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        });
        if well_formed {
            candidate
        } else {
            String::new()
        }
    }
}
