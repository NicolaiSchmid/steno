//! Person pages: created whole when missing, otherwise only the managed
//! block is Steno's.
//! Swift: `Sources/StenoAdapters/Rendering/PersonPageRenderer.swift`.

use steno_core::{MeetingExport, Person};

use super::{
    Frontmatter, FrontmatterValue, LinkStyle, ManagedBlock, PersonPage, RenderOptions, date_text,
    markdown_text,
};
use crate::naming::{MeetingFolder, Note, Slug};

pub(crate) struct PersonPageRenderer<'a> {
    pub export: &'a MeetingExport,
    pub options: &'a RenderOptions,
    pub folder_slug: &'a str,
}

impl PersonPageRenderer<'_> {
    /// The page as created from scratch plus this meeting's line. The file is
    /// named after the display name, sanitised the way
    /// [`markdown_text::wikilink`] sanitises its target, so `[[Anna Müller]]`
    /// resolves to it.
    pub fn page(&self, person: &Person) -> PersonPage {
        let line = self.line();
        let mut frontmatter = Frontmatter::new(self.options.time_zone);
        frontmatter.append(
            "steno_person_id",
            FrontmatterValue::String(person.id.to_string()),
        );
        if let Some(email) = person.email.as_deref().filter(|email| !email.is_empty()) {
            frontmatter.append("email", FrontmatterValue::String(email.to_owned()));
        }
        frontmatter.append("type", FrontmatterValue::String("person".to_owned()));
        let page = [
            frontmatter.encoded(),
            format!("# {}\n", markdown_text::single_line(&person.display_name)),
            ManagedBlock::block(std::slice::from_ref(&line)),
        ]
        .join("\n");
        PersonPage {
            file_name: format!("{}.md", Slug::file_name(&person.display_name)),
            page,
            line,
        }
    }

    /// `- 2026-09-24 [[2026-09-24-slug|Title]] %%steno:<meeting uuid>%%`, or
    /// with a root-absolute Markdown link to the folder note in `None` style.
    pub fn line(&self) -> String {
        let meeting = &self.export.meeting;
        let link = match self.options.link_style {
            LinkStyle::Wikilink => markdown_text::wikilink(self.folder_slug, Some(&meeting.title)),
            LinkStyle::None => markdown_text::markdown_link(
                &meeting.title,
                &format!(
                    "/{}/{}/{}",
                    MeetingFolder::ROOT,
                    self.folder_slug,
                    MeetingFolder::note_file(Note::Folder, self.folder_slug)
                ),
            ),
        };
        format!(
            "- {} {link} {}",
            date_text::day(meeting.started_at, self.options.time_zone),
            ManagedBlock::marker(meeting.id)
        )
    }
}
