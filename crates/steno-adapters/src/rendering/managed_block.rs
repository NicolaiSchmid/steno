//! The region of a person page that Steno owns, as text; the Obsidian
//! destination writes it.
//! Swift: `Sources/StenoAdapters/Obsidian/ManagedBlock.swift`.

use uuid::Uuid;

/// One line per meeting between two HTML comments, newest first, each line
/// ending in a `%%steno:<uuid>%%` comment that identifies its meeting.
/// [`ManagedBlock::merge`] replaces or inserts a meeting's line,
/// [`ManagedBlock::remove`] drops it again when the person left the meeting
/// (a speaker reassigned to somebody else).
///
/// Only lines that carry a `%%steno:` marker are managed: they are the
/// ones re-sorted and replaced. Every other non-blank line inside the
/// block keeps its bytes and its place, and bytes outside the markers are
/// copied unchanged. The block is the first start marker and the first end
/// marker after it; a page without an end marker after the start (the user
/// deleted it), or with the end before the start, has no block: `merge`
/// appends a fresh one and `remove` changes nothing. A second start marker
/// inside the block is an ordinary line.
///
/// ```
/// use steno_adapters::rendering::ManagedBlock;
/// use uuid::Uuid;
///
/// let id = Uuid::nil();
/// let line = format!("- 2026-09-24 [[2026-09-24-sync|Sync]] {}", ManagedBlock::marker(id));
/// let page = ManagedBlock::merge(&line, id, "# Anna\n");
/// assert_eq!(
///     page,
///     "# Anna\n\n<!-- steno:meetings:start -->\n- 2026-09-24 [[2026-09-24-sync|Sync]] %%steno:00000000-0000-0000-0000-000000000000%%\n<!-- steno:meetings:end -->\n"
/// );
/// assert_eq!(ManagedBlock::merge(&line, id, &page), page, "idempotent");
/// assert_eq!(ManagedBlock::remove(id, &page), "# Anna\n\n<!-- steno:meetings:start -->\n<!-- steno:meetings:end -->\n");
/// ```
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
    /// marker (or inserted when there is none) and the marker lines re-sorted
    /// newest first where the first of them stood; the block appended when
    /// there is none. Lines without a marker keep their bytes and their
    /// place (a CRLF page keeps its `\r`s); blank lines inside the block are
    /// dropped.
    #[must_use]
    pub fn merge(line: &str, meeting_id: Uuid, existing: &str) -> String {
        let Some((start, end)) = Self::body_range(existing) else {
            // A blank line separates the block from the page's last line.
            let mut result = existing.to_owned();
            if !result.is_empty() {
                if !result.ends_with('\n') {
                    result.push('\n');
                }
                result.push('\n');
            }
            result.push_str(&Self::block(&[line.to_owned()]));
            return result;
        };
        let marker = Self::marker(meeting_id);
        let mut kept: Vec<String> = Vec::new();
        let mut managed: Vec<String> = Vec::new();
        let mut slot = None;
        for candidate in existing[start..end]
            .split('\n')
            .filter(|candidate| !candidate.trim_matches([' ', '\t', '\r']).is_empty())
        {
            if Self::is_managed(candidate) {
                slot.get_or_insert(kept.len());
                if !candidate.contains(&marker) {
                    managed.push(candidate.to_owned());
                }
            } else {
                kept.push(candidate.to_owned());
            }
        }
        managed.push(line.to_owned());
        let slot = slot.unwrap_or(0);
        kept.splice(slot..slot, Self::sorted_newest_first(managed));
        format!(
            "{}\n{}\n{}",
            &existing[..start],
            kept.join("\n"),
            &existing[end..]
        )
    }

    /// Whether a line is one of ours: it carries a `%%steno:` marker.
    fn is_managed(line: &str) -> bool {
        line.contains("%%steno:")
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

    /// `"2026-09-24"` from `- 2026-09-24 …`; `None` when fewer than ten
    /// characters follow the bullet or they do not fit `DDDD-DD-DD`.
    fn date_of(line: &str) -> Option<&str> {
        let candidate = line.trim_start_matches(['-', ' ', '*']).get(..10)?;
        let well_formed = candidate
            .bytes()
            .enumerate()
            .all(|(index, byte)| match index {
                4 | 7 => byte == b'-',
                _ => byte.is_ascii_digit(),
            });
        well_formed.then_some(candidate)
    }
}
