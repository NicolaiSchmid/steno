//! Speaker and person names as the renderers write them.
//! Swift: `Sources/StenoAdapters/Rendering/Names.swift`.

use std::collections::HashSet;

use steno_core::{MeetingExport, MeetingTask, Speaker};
use uuid::Uuid;

use super::RenderOptions;
use super::markdown_text;

/// The current display name from the export, wikilinked only when the
/// options link people and the name belongs to a `Person` (who has a page),
/// never for a cluster label or a participant the app knows by name alone.
pub(crate) struct Names<'a> {
    pub export: &'a MeetingExport,
    pub options: &'a RenderOptions,
}

impl Names<'_> {
    /// The display name of a segment's speaker; `"Unknown"` for none.
    pub fn speaker(&self, speaker_id: Option<Uuid>) -> String {
        match speaker_id {
            Some(id) => self.export.display_name_for_speaker(id),
            None => "Unknown".to_owned(),
        }
    }

    /// The speaker's name, linked when it resolves to a person.
    pub fn linked_speaker(&self, speaker_id: Option<Uuid>) -> String {
        let name = self.speaker(speaker_id);
        let is_person = speaker_id
            .and_then(|id| self.export.speaker(id))
            .and_then(Speaker::person_id)
            .is_some();
        if is_person { self.person(&name) } else { name }
    }

    /// A person's name, linked when the options link people.
    pub fn person(&self, display_name: &str) -> String {
        if self.options.links_people() {
            markdown_text::wikilink(display_name, None)
        } else {
            display_name.to_owned()
        }
    }

    /// A task's assignee, linked when it resolves to a person: through
    /// `assignee_person_id`, or through a speaker whose cluster label the
    /// model used as the name (it sees labels, as the summary does), else the
    /// name the model wrote, plain.
    pub fn assignee(&self, task: &MeetingTask) -> Option<String> {
        if let Some(person) = task
            .assignee_person_id
            .and_then(|id| self.export.person(id))
        {
            return Some(self.person(&person.display_name));
        }
        let name = task
            .assignee_name
            .as_deref()
            .filter(|name| !name.is_empty())?;
        if let Some(speaker) = self
            .export
            .speakers
            .iter()
            .find(|speaker| speaker.cluster_label == name)
            && speaker.person_id().is_some()
        {
            return Some(self.person(&self.export.display_name_for_speaker(speaker.id)));
        }
        Some(name.to_owned())
    }

    /// Everybody in the meeting, once each: participants in export order
    /// (linked when they are a person), then speakers that resolved to nobody
    /// under their cluster label.
    pub fn participants(&self) -> Vec<String> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut names = Vec::new();
        for participant in &self.export.participants {
            if !seen.insert(participant.display_name.clone()) {
                continue;
            }
            names.push(if participant.person_id.is_none() {
                participant.display_name.clone()
            } else {
                self.person(&participant.display_name)
            });
        }
        for speaker in &self.export.speakers {
            let name = self.export.display_name_for_speaker(speaker.id);
            if !seen.insert(name.clone()) {
                continue;
            }
            names.push(if speaker.person_id().is_none() {
                name
            } else {
                self.person(&name)
            });
        }
        names
    }
}
