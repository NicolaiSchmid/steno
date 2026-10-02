//! The speakers of one meeting as the header row, the popover and the
//! transcript pickers show them, after `Speakers/SpeakersViewModel.swift`
//! and the core's `SpeakerOptions` and `SpeakerExcerpts`: one row per
//! speaker from the current export, the ranked options a picker offers, the
//! excerpt a row shows, and the one write, [`SpeakersViewModel::select`].

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use steno_core::{
    MeetingExport, ParticipantRole, Person, Speaker, SpeakerAssignment, SpeakerNameSuggestion,
    Store, TranscriptSegment, fold_name, paths::path_from_file_url,
};
use uuid::Uuid;

use crate::services::{ClipPlayer, FileSystem};

/// What choosing a row does. Swift: `SpeakerOptions.Option.Kind`.
#[derive(Debug, Clone, PartialEq)]
pub enum OptionKind {
    /// Confirms the speaker as this person.
    Person(Person),
    /// Resolves the name through `Store::resolve_person` and confirms.
    Create(String),
}

/// The trailing label that says why the row is offered. Swift:
/// `SpeakerOptions.Option.Tag`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionTag {
    SoundsLike,
    Mentioned,
    Attendee,
    InThisMeeting,
}

impl OptionTag {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            OptionTag::SoundsLike => "Sounds like",
            OptionTag::Mentioned => "Mentioned",
            OptionTag::Attendee => "Attendee",
            OptionTag::InThisMeeting => "In this meeting",
        }
    }
}

/// One row of the picker. Swift: `SpeakerOptions.Option`.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeakerOption {
    pub kind: OptionKind,
    pub tag: Option<OptionTag>,
}

impl SpeakerOption {
    /// The person's name, or the text a Create row would create.
    #[must_use]
    pub fn display_name(&self) -> &str {
        match &self.kind {
            OptionKind::Person(person) => &person.display_name,
            OptionKind::Create(text) => text,
        }
    }

    #[must_use]
    pub fn person(&self) -> Option<&Person> {
        match &self.kind {
            OptionKind::Person(person) => Some(person),
            OptionKind::Create(_) => None,
        }
    }
}

/// Case- and diacritic-insensitive substring test. Swift: `SpeakerOptions.contains`.
#[must_use]
pub fn name_contains(text: &str, query: &str) -> bool {
    fold_name(text).contains(&fold_name(query))
}

struct OptionList {
    query: String,
    excluded_person_id: Option<Uuid>,
    options: Vec<SpeakerOption>,
    listed_person_ids: Vec<Uuid>,
}

impl OptionList {
    /// Appends the option unless the person is excluded or already listed,
    /// an option of the same name is already listed (create rows), or the
    /// query filters it out.
    fn add(&mut self, kind: OptionKind, tag: Option<OptionTag>) {
        match &kind {
            OptionKind::Person(person) => {
                if Some(person.id) == self.excluded_person_id
                    || self.listed_person_ids.contains(&person.id)
                {
                    return;
                }
            }
            OptionKind::Create(text) => {
                if self.contains_name(text) {
                    return;
                }
            }
        }
        let option = SpeakerOption { kind, tag };
        if !self.query.is_empty() && !name_contains(option.display_name(), &self.query) {
            return;
        }
        if let OptionKind::Person(person) = &option.kind {
            self.listed_person_ids.push(person.id);
        }
        self.options.push(option);
    }

    fn contains_name(&self, name: &str) -> bool {
        self.options
            .iter()
            .any(|option| Person::names_match(option.display_name(), name))
    }
}

/// Builds the ranked list for `speaker`: the voice match ("Sounds like"),
/// the LLM guess ("Mentioned"), unassigned calendar attendees ("Attendee"),
/// persons owning another speaker of this meeting ("In this meeting"),
/// recent persons, then, once the query is non-empty, every other person
/// whose name contains it, and a Create row last. Each person appears at
/// most once and a confirmed speaker's own person is never offered.
/// Swift: `SpeakerOptions.build`.
#[must_use]
pub fn build_options(
    speaker: &Speaker,
    speakers: &[Speaker],
    persons: &[Person],
    participants: &[steno_core::Participant],
    suggestion: Option<&SpeakerNameSuggestion>,
    recent: &[Person],
    query: &str,
) -> Vec<SpeakerOption> {
    let query = query.trim();
    let excluded = match speaker.assignment {
        SpeakerAssignment::Confirmed { person_id } => Some(person_id),
        _ => None,
    };
    let mut list = OptionList {
        query: query.to_owned(),
        excluded_person_id: excluded,
        options: Vec::new(),
        listed_person_ids: Vec::new(),
    };
    let person_by_id = |id: Uuid| persons.iter().find(|person| person.id == id).cloned();
    let others_person_ids: Vec<Uuid> = speakers
        .iter()
        .filter(|other| other.id != speaker.id)
        .filter_map(Speaker::person_id)
        .collect();

    if let SpeakerAssignment::Suggested { person_id, .. } = speaker.assignment
        && let Some(person) = person_by_id(person_id)
    {
        list.add(OptionKind::Person(person), Some(OptionTag::SoundsLike));
    }

    if let Some(name) = suggestion
        .and_then(|suggestion| suggestion.name.as_deref())
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        if let Some(person) = persons
            .iter()
            .find(|person| Person::names_match(&person.display_name, name))
        {
            list.add(
                OptionKind::Person(person.clone()),
                Some(OptionTag::Mentioned),
            );
        } else {
            list.add(
                OptionKind::Create(name.to_owned()),
                Some(OptionTag::Mentioned),
            );
        }
    }

    for participant in participants
        .iter()
        .filter(|participant| participant.role == ParticipantRole::Them)
    {
        let name = participant.display_name.trim();
        if name.is_empty() || list.contains_name(name) {
            continue;
        }
        let person = participant.person_id.and_then(person_by_id).or_else(|| {
            persons
                .iter()
                .find(|person| Person::names_match(&person.display_name, name))
                .cloned()
        });
        match person {
            Some(person) => {
                if others_person_ids.contains(&person.id) {
                    continue;
                }
                list.add(OptionKind::Person(person), Some(OptionTag::Attendee));
            }
            None => list.add(
                OptionKind::Create(name.to_owned()),
                Some(OptionTag::Attendee),
            ),
        }
    }

    for other in speakers.iter().filter(|other| other.id != speaker.id) {
        if let Some(person) = other.person_id().and_then(person_by_id) {
            list.add(OptionKind::Person(person), Some(OptionTag::InThisMeeting));
        }
    }

    for person in recent {
        list.add(OptionKind::Person(person.clone()), None);
    }

    if !query.is_empty() {
        for person in persons
            .iter()
            .filter(|person| name_contains(&person.display_name, query))
        {
            list.add(OptionKind::Person(person.clone()), None);
        }
        if !list.contains_name(query) {
            list.options.push(SpeakerOption {
                kind: OptionKind::Create(query.to_owned()),
                tag: None,
            });
        }
    }
    list.options
}

/// Characters kept before an excerpt is cut and "…" appended.
pub const EXCERPT_MAX_LENGTH: usize = 160;

/// What a speaker said in their sample clip: the speaker's segments
/// overlapping the clip range joined with a space, else their longest
/// segment; "" when no segment belongs to it. A leading "…" marks text
/// that starts before the clip; text longer than [`EXCERPT_MAX_LENGTH`] is
/// cut there and ends with "…". Swift: `SpeakerExcerpts.text`.
#[must_use]
pub fn excerpt(speaker: &Speaker, segments: &[TranscriptSegment]) -> String {
    let own: Vec<&TranscriptSegment> = segments
        .iter()
        .filter(|segment| segment.speaker_id == Some(speaker.id))
        .collect();
    if own.is_empty() {
        return String::new();
    }
    let mut used: Vec<&TranscriptSegment> = Vec::new();
    let mut cut_at_start = false;
    if let Some(range) = speaker.sample_clip_range {
        used = own
            .iter()
            .copied()
            .filter(|segment| segment.start < range.upper && segment.end > range.lower)
            .collect();
        used.sort_by(|left, right| left.start.total_cmp(&right.start));
        if used.first().is_some_and(|first| first.start < range.lower) {
            cut_at_start = true;
        }
    }
    if used.is_empty() {
        let longest = own
            .iter()
            .copied()
            .max_by(|left, right| left.duration().total_cmp(&right.duration()))
            .expect("own is not empty");
        used = vec![longest];
        cut_at_start = false;
    }
    let mut text = used
        .iter()
        .map(|segment| segment.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if text.chars().count() > EXCERPT_MAX_LENGTH {
        text = text.chars().take(EXCERPT_MAX_LENGTH).collect::<String>();
        text = format!("{}…", text.trim());
    }
    if cut_at_start && !text.is_empty() {
        text = format!("…{text}");
    }
    text
}

/// One speaker as the rows show it. Swift: `SpeakersViewModel.Row`.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeakerRow {
    pub speaker: Speaker,
    /// The confirmed or suggested person, when the export has them.
    pub person: Option<Person>,
    /// What the speaker said in the clip range; empty for confirmed rows.
    pub excerpt: String,
    /// Whether the sample clip file exists.
    pub can_play: bool,
}

impl SpeakerRow {
    #[must_use]
    pub fn id(&self) -> Uuid {
        self.speaker.id
    }

    #[must_use]
    pub fn is_confirmed(&self) -> bool {
        self.speaker.assignment.is_confirmed()
    }

    /// The confirmed or suggested person's name, else the cluster label.
    #[must_use]
    pub fn display_name(&self) -> &str {
        self.person
            .as_ref()
            .map_or(self.speaker.cluster_label.as_str(), |person| {
                person.display_name.as_str()
            })
    }
}

/// The clip file of a speaker, when it names one.
#[must_use]
pub fn clip_path(speaker: &Speaker) -> Option<PathBuf> {
    speaker
        .sample_clip_url
        .as_deref()
        .and_then(path_from_file_url)
}

/// Swift: `SpeakersViewModel`.
#[derive(Debug, Default)]
pub struct SpeakersViewModel {
    pub rows: Vec<SpeakerRow>,
    pub export: Option<MeetingExport>,
    pub recent: Vec<Person>,
    pub persons: Vec<Person>,
    pub suggestions: BTreeMap<Uuid, SpeakerNameSuggestion>,
    pub error: Option<String>,
}

impl SpeakersViewModel {
    #[must_use]
    pub fn unconfirmed_count(&self) -> usize {
        self.rows.iter().filter(|row| !row.is_confirmed()).count()
    }

    #[must_use]
    pub fn row(&self, id: Uuid) -> Option<&SpeakerRow> {
        self.rows.iter().find(|row| row.id() == id)
    }

    /// A new export from the store: rows are rebuilt and the name
    /// suggestions, the recent people and the full people list reloaded.
    /// Swift ran the three reads in a background task; here they are three
    /// store reads in a row.
    pub fn update(&mut self, export: MeetingExport, store: &Store, files: &dyn FileSystem) {
        self.rows = export
            .speakers
            .iter()
            .map(|speaker| SpeakerRow {
                person: speaker
                    .person_id()
                    .and_then(|id| export.persons.iter().find(|person| person.id == id))
                    .cloned(),
                excerpt: if speaker.assignment.is_confirmed() {
                    String::new()
                } else {
                    excerpt(speaker, &export.segments)
                },
                can_play: clip_path(speaker).is_some_and(|path| files.exists(&path)),
                speaker: speaker.clone(),
            })
            .collect();
        let meeting_id = export.meeting.id;
        self.export = Some(export);
        let loaded = (|| -> steno_core::store::Result<()> {
            let suggestions = store.name_suggestions(meeting_id)?;
            let recent = store.recent_persons()?;
            let persons = store.persons()?;
            self.suggestions = suggestions
                .into_iter()
                .map(|suggestion| (suggestion.speaker_id, suggestion))
                .collect();
            self.recent = recent;
            self.persons = persons;
            Ok(())
        })();
        if let Err(error) = loaded {
            self.error = Some(format!("People could not be loaded: {error}"));
        }
    }

    /// The suggested person's name for a `suggested` speaker, what the
    /// field opens with; `None` otherwise.
    #[must_use]
    pub fn prefill(&self, speaker_id: Uuid) -> Option<String> {
        let row = self.row(speaker_id)?;
        match row.speaker.assignment {
            SpeakerAssignment::Suggested { .. } => row
                .person
                .as_ref()
                .map(|person| person.display_name.clone()),
            _ => None,
        }
    }

    /// The ranked options for one speaker's picker. Reads only.
    #[must_use]
    pub fn options(&self, speaker_id: Uuid, query: &str) -> Vec<SpeakerOption> {
        let Some(export) = &self.export else {
            return Vec::new();
        };
        let Some(speaker) = export
            .speakers
            .iter()
            .find(|speaker| speaker.id == speaker_id)
        else {
            return Vec::new();
        };
        let mut known = self.persons.clone();
        for person in &export.persons {
            if !known.iter().any(|candidate| candidate.id == person.id) {
                known.push(person.clone());
            }
        }
        let suggestion = self.suggestions.get(&speaker_id).filter(|suggestion| {
            suggestion
                .name
                .as_deref()
                .is_some_and(|name| !name.is_empty())
        });
        build_options(
            speaker,
            &export.speakers,
            &known,
            &export.participants,
            suggestion,
            &self.recent,
            query,
        )
    }

    /// The one write: a person option confirms that person (merging when
    /// they already own another speaker here); a create option resolves
    /// the name to an existing or new person first, taking the email of a
    /// calendar attendee of that name. Selecting the speaker's own person
    /// again changes nothing. A speaker the export does not list is
    /// ignored. Returns whether a speaker changed (the owner schedules the
    /// re-export then).
    pub fn select(
        &mut self,
        option: &SpeakerOption,
        speaker_id: Uuid,
        store: &Store,
        now: DateTime<Utc>,
    ) -> bool {
        let Some(row) = self.row(speaker_id).cloned() else {
            return false;
        };
        let outcome = (|| -> steno_core::store::Result<bool> {
            let person = match &option.kind {
                OptionKind::Person(known) => known.clone(),
                OptionKind::Create(name) => {
                    let attendee = self.export.as_ref().and_then(|export| {
                        export.participants.iter().find(|participant| {
                            participant.role == ParticipantRole::Them
                                && Person::names_match(&participant.display_name, name)
                        })
                    });
                    store.resolve_person(
                        name,
                        attendee.and_then(|attendee| attendee.email.as_deref()),
                        now,
                    )?
                }
            };
            if row.speaker.assignment
                == (SpeakerAssignment::Confirmed {
                    person_id: person.id,
                })
            {
                return Ok(false);
            }
            let clips = store.confirm_speaker(speaker_id, &person)?;
            for clip in clips.iter().filter_map(|clip| path_from_file_url(clip)) {
                let _ = std::fs::remove_file(clip);
            }
            Ok(true)
        })();
        match outcome {
            Ok(wrote) => wrote,
            Err(error) => {
                self.error = Some(format!("Speaker could not be named: {error}"));
                false
            }
        }
    }

    // Playback

    pub fn play(&mut self, id: Uuid, player: &dyn ClipPlayer) {
        let Some(path) = self.row(id).and_then(|row| clip_path(&row.speaker)) else {
            self.error = Some("This speaker has no sample clip.".to_owned());
            return;
        };
        if !player.play(&path) {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.error = Some(format!("The sample clip could not be played ({name})."));
        }
    }

    pub fn stop_playback(&self, player: &dyn ClipPlayer) {
        player.stop();
    }

    /// The row whose clip is playing.
    #[must_use]
    pub fn playing(&self, player: &dyn ClipPlayer) -> Option<Uuid> {
        let playing = player.playing()?;
        self.rows
            .iter()
            .find(|row| clip_path(&row.speaker).as_deref() == Some(playing.as_path()))
            .map(SpeakerRow::id)
    }
}
