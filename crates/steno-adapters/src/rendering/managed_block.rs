//! The region of a person page that Steno owns.

use uuid::Uuid;

/// One line per meeting between two HTML comments, newest first, each
/// ending in a `%%steno:<uuid>%%` marker. Only marked lines are managed;
/// other lines and bytes outside the markers keep their bytes. The block is
/// the first start marker and the first end marker after it; without one,
/// `merge` appends a fresh block and `remove` changes nothing.
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

    /// `%%steno:<uuid>%%`, Obsidian's comment syntax so the id never shows.
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

    /// The byte range of the block body.
    fn body_range(existing: &str) -> Option<(usize, usize)> {
        let start = existing.find(Self::START)? + Self::START.len();
        let end = start + existing[start..].find(Self::END)?;
        Some((start, end))
    }

    /// `existing` with `line` replacing or inserted for this meeting's
    /// marker, the marked lines re-sorted newest first; a block appended if
    /// none. Unmarked lines keep their bytes and place; blank lines inside
    /// are dropped.
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

    fn is_managed(line: &str) -> bool {
        line.contains("%%steno:")
    }

    /// `existing` without this meeting's line; unchanged when there is no
    /// block or no such line. An emptied block keeps its markers.
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

    /// By leading `- YYYY-MM-DD` descending, then text; undated lines last.
    #[must_use]
    pub fn sorted_newest_first(mut lines: Vec<String>) -> Vec<String> {
        lines.sort_by(|left, right| {
            Self::date_of(right)
                .cmp(&Self::date_of(left))
                .then_with(|| left.cmp(right))
        });
        lines
    }

    /// The `YYYY-MM-DD` after the bullet, if it fits the pattern.
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
