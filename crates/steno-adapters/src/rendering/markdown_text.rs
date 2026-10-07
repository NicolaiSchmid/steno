//! Escaping and link helpers shared by the Markdown renderers.
//! Swift: `Sources/StenoAdapters/Rendering/MarkdownText.swift`.

use crate::naming::Slug;

/// `[[Anna Müller]]` or `[[2026-09-24-slug|Title]]`. The target goes
/// through [`Slug::file_name`] (a link names a note file) and the alias
/// loses `|`, `]]` and line breaks so it cannot break out of the link. A
/// target the device-name rule renamed (`Con` to `Con_` on Windows) shows
/// the name as written: `[[Con_|Con]]`.
#[must_use]
pub fn wikilink(target: &str, alias: Option<&str>) -> String {
    let name = Slug::file_name(target);
    match alias {
        Some(alias) if !alias.is_empty() => format!("[[{name}|{}]]", link_alias(alias)),
        _ => {
            let written = Slug::file_name_reserving(target, false);
            if written == name {
                format!("[[{name}]]")
            } else {
                format!("[[{name}|{}]]", link_alias(&written))
            }
        }
    }
}

/// `[Transcript](<2026-09-24-slug - Transcript.md>)`: the `CommonMark` form
/// with an angle-bracketed destination so spaces need no encoding.
#[must_use]
pub fn markdown_link(text: &str, file: &str) -> String {
    let destination = file.replace('>', "%3E").replace('<', "%3C");
    format!("[{}](<{destination}>)", link_alias(text))
}

#[must_use]
pub fn link_alias(text: &str) -> String {
    single_line(text)
        .replace("]]", "]\u{200B}]")
        .replace('|', "/")
        .replace('[', "(")
        .replace(']', ")")
}

/// A paragraph that would otherwise start a heading, list item, block
/// quote or ordered list (`#`, `-`, `>`, `1.`) gets a backslash.
#[must_use]
pub fn escape_paragraph_start(paragraph: &str) -> String {
    let Some(first) = paragraph.chars().next() else {
        return paragraph.to_owned();
    };
    if matches!(first, '#' | '-' | '>') {
        return format!("\\{paragraph}");
    }
    let digits = paragraph.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && paragraph.as_bytes().get(digits) == Some(&b'.') {
        return format!("\\{paragraph}");
    }
    paragraph.to_owned()
}

/// Line breaks and runs of whitespace become one space; trimmed.
#[must_use]
pub fn single_line(text: &str) -> String {
    text.split(char::is_whitespace)
        .filter(|piece| !piece.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// An Obsidian tag without `#`: whitespace to `-`, only `[A-Za-z0-9_/-]`
/// kept, separators trimmed. `None` when nothing is left.
#[must_use]
pub fn tag(raw: &str) -> Option<String> {
    let tag = raw
        .split(char::is_whitespace)
        .map(|piece| {
            piece
                .chars()
                .filter(|c| is_tag_character(*c))
                .collect::<String>()
        })
        .filter(|piece| !piece.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .trim_matches(['-', '/', '_'])
        .to_owned();
    (!tag.is_empty()).then_some(tag)
}

fn is_tag_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '_' | '/' | '-')
}
