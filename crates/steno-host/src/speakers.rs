//! The speakers of one meeting as the header row, the popover and the
//! transcript pickers show them: one row per speaker, the ranked options a
//! picker offers, the excerpt a row shows, and the writes: naming
//! ([`SpeakersViewModel::select`]), No to a suggestion, Remove name, and
//! Not a person.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use steno_core::{
    MeetingExport, ParticipantRole, Person, Speaker, SpeakerAssignment, SpeakerNameSuggestion,
    Store, TranscriptSegment, VoiceStrength, fold_name, me_speaker_id, paths::file_url_path,
};
use uuid::Uuid;

use crate::services::{ClipPlayer, FileSystem};
use crate::settings::update_settings;

/// What choosing a row does.
#[derive(Debug, Clone, PartialEq)]
pub enum OptionKind {
    /// Confirms the speaker as this person.
    Person(Person),
    /// Resolves the name through `Store::resolve_person` and confirms.
    Create(String),
}

/// Why the row is offered. Only `Voice` is voice evidence, and only it
/// carries a strength.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionTag {
    /// The voice matches this person's voice memory.
    Voice(VoiceStrength),
    /// The summary heard the name for this speaker.
    Mentioned,
    /// A calendar invitee nobody's voice is named as.
    Invited,
    /// The only invitee left for the only voice left unnamed.
    LastInvitee,
    /// The person owns another voice of this meeting.
    InThisMeeting,
}

/// One row of the picker.
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

/// Case- and diacritic-insensitive substring test.
#[must_use]
pub fn name_contains(text: &str, query: &str) -> bool {
    fold_name(text).contains(&fold_name(query))
}

struct OptionList<'a> {
    query: String,
    excluded_person_id: Option<Uuid>,
    /// The persons the user said No to for this voice, and their names:
    /// never suggested again, offered only when typed.
    rejected: Vec<&'a Person>,
    options: Vec<SpeakerOption>,
    listed_person_ids: Vec<Uuid>,
}

impl OptionList<'_> {
    /// Appends the option unless the person is excluded or already listed,
    /// an option of the same name is already listed (create rows), the user
    /// said No to them for this voice and nothing is typed, or the query
    /// filters it out. A person the user said No to comes back, when typed,
    /// without a reason.
    fn add(&mut self, kind: OptionKind, tag: Option<OptionTag>) {
        let tag = tag.filter(|_| {
            let name = match &kind {
                OptionKind::Person(person) => &person.display_name,
                OptionKind::Create(text) => text,
            };
            !self.is_rejected(name)
        });
        match &kind {
            OptionKind::Person(person) => {
                if Some(person.id) == self.excluded_person_id
                    || self.listed_person_ids.contains(&person.id)
                    || (self.query.is_empty() && self.is_rejected(&person.display_name))
                {
                    return;
                }
            }
            OptionKind::Create(text) => {
                if self.contains_name(text) || (self.query.is_empty() && self.is_rejected(text)) {
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

    /// The invitees not yet offered, tagged `invited`. One already offered
    /// as mentioned takes `LastInvitee`, the stronger reason; a voice match
    /// stays one.
    fn add_invitees(&mut self, invitees: Vec<OptionKind>, invited: OptionTag) {
        for invitee in invitees {
            let name = match &invitee {
                OptionKind::Person(person) => &person.display_name,
                OptionKind::Create(name) => name,
            };
            if let Some(listed) = self
                .options
                .iter_mut()
                .find(|option| Person::names_match(option.display_name(), name))
            {
                if invited == OptionTag::LastInvitee && listed.tag == Some(OptionTag::Mentioned) {
                    listed.tag = Some(invited);
                }
            } else {
                self.add(invitee, Some(invited));
            }
        }
    }

    fn contains_name(&self, name: &str) -> bool {
        self.options
            .iter()
            .any(|option| Person::names_match(option.display_name(), name))
    }

    fn is_rejected(&self, name: &str) -> bool {
        self.rejected
            .iter()
            .any(|person| Person::names_match(&person.display_name, name))
    }
}

/// The meeting's voices still to name: neither confirmed nor dismissed,
/// and not "me", which no invitee is.
fn open_voices(speakers: &[Speaker]) -> impl Iterator<Item = &Speaker> {
    speakers.iter().filter(|speaker| {
        !speaker.dismissed
            && !speaker.assignment.is_confirmed()
            && speaker.id != me_speaker_id(speaker.meeting_id)
    })
}

/// The meeting's invitees nobody's voice is named or suggested as here,
/// and not answered No for this voice, as a person when Steno knows them;
/// each once, however many participant rows name them.
fn invitees(
    participants: &[steno_core::Participant],
    persons: &[Person],
    others_person_ids: &[Uuid],
    rejected: &BTreeSet<Uuid>,
) -> Vec<OptionKind> {
    participants
        .iter()
        .filter(|participant| participant.role == ParticipantRole::Them)
        .filter_map(|participant| {
            let name = participant.display_name.trim();
            if name.is_empty() {
                return None;
            }
            let person = participant
                .person_id
                .and_then(|id| persons.iter().find(|person| person.id == id))
                .or_else(|| {
                    persons
                        .iter()
                        .find(|person| Person::names_match(&person.display_name, name))
                })
                .cloned();
            match person {
                Some(person) if others_person_ids.contains(&person.id) => None,
                Some(person) if rejected.contains(&person.id) => None,
                Some(person) => Some(OptionKind::Person(person)),
                None => Some(OptionKind::Create(name.to_owned())),
            }
        })
        .fold(Vec::new(), |mut unique: Vec<OptionKind>, invitee| {
            let name = |kind: &OptionKind| match kind {
                OptionKind::Person(person) => person.display_name.clone(),
                OptionKind::Create(name) => name.clone(),
            };
            if !unique
                .iter()
                .any(|listed| Person::names_match(&name(listed), &name(&invitee)))
            {
                unique.push(invitee);
            }
            unique
        })
}

/// What [`build_options`] ranks from besides the speaker and the query.
pub struct OptionSources<'a> {
    /// Every speaker of the meeting.
    pub speakers: &'a [Speaker],
    pub persons: &'a [Person],
    pub participants: &'a [steno_core::Participant],
    /// The summary's name guess for this speaker.
    pub suggestion: Option<&'a SpeakerNameSuggestion>,
    pub recent: &'a [Person],
    /// The persons the user said No to for this speaker.
    pub rejected: &'a BTreeSet<Uuid>,
}

/// Builds the ranked list for `speaker`: the voice match (`Voice`, with its
/// strength, from [`VoiceStrength::FLOOR`] up), the summary's guess
/// (`Mentioned`), invitees nobody is named as (`Invited`, or `LastInvitee`
/// when this is the meeting's only open voice and they are the only
/// invitee left), persons owning another speaker here (`InThisMeeting`),
/// recent persons, then, for a non-empty query, every other matching
/// person and a Create row. Each person appears once; a confirmed
/// speaker's own person is never offered, and a person the user said No to
/// for this voice only when typed.
#[must_use]
pub fn build_options(
    speaker: &Speaker,
    sources: &OptionSources<'_>,
    query: &str,
) -> Vec<SpeakerOption> {
    let OptionSources {
        speakers,
        persons,
        participants,
        suggestion,
        recent,
        rejected,
    } = *sources;
    let query = query.trim();
    let excluded = match speaker.assignment {
        SpeakerAssignment::Confirmed { person_id } => Some(person_id),
        _ => None,
    };
    let mut list = OptionList {
        query: query.to_owned(),
        excluded_person_id: excluded,
        rejected: persons
            .iter()
            .filter(|person| rejected.contains(&person.id))
            .collect(),
        options: Vec::new(),
        listed_person_ids: Vec::new(),
    };
    let person_by_id = |id: Uuid| persons.iter().find(|person| person.id == id).cloned();
    let others_person_ids: Vec<Uuid> = speakers
        .iter()
        .filter(|other| other.id != speaker.id)
        .filter_map(Speaker::person_id)
        .collect();

    if let SpeakerAssignment::Suggested {
        person_id,
        similarity,
    } = speaker.assignment
        && let Some(strength) = VoiceStrength::of(similarity)
        && let Some(person) = person_by_id(person_id)
    {
        list.add(OptionKind::Person(person), Some(OptionTag::Voice(strength)));
    }

    if let Some(name) = suggestion
        .and_then(|suggestion| suggestion.name.as_deref())
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        let mentioned = persons
            .iter()
            .find(|person| Person::names_match(&person.display_name, name))
            .map_or_else(
                || OptionKind::Create(name.to_owned()),
                |person| OptionKind::Person(person.clone()),
            );
        list.add(mentioned, Some(OptionTag::Mentioned));
    }

    let invitees = invitees(participants, persons, &others_person_ids, rejected);
    let mut open = open_voices(speakers);
    let last_voice = open.next().is_some_and(|only| only.id == speaker.id) && open.next().is_none();
    let invited = if last_voice && invitees.len() == 1 {
        OptionTag::LastInvitee
    } else {
        OptionTag::Invited
    };
    list.add_invitees(invitees, invited);

    for other in speakers
        .iter()
        .filter(|other| other.id != speaker.id && !other.dismissed)
    {
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

/// What a speaker's row shows of what they said.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Excerpt {
    pub text: String,
    /// Whether `text` is exactly what the sample clip plays: every segment
    /// used lies inside the clip. Segments carry no word timings, so a
    /// segment only partly inside the clip makes the text context around
    /// it, as does the longest segment of a voice without a clip.
    pub in_clip: bool,
}

/// Slack at the clip's edges for a segment to count as inside it, in
/// seconds.
const CLIP_EDGE: f64 = 0.25;

/// What a speaker says in their sample clip: their segments overlapping the
/// clip range, a quote when all of them lie inside it ([`Excerpt::in_clip`]);
/// without a clip range (or, which the diarizer never produces, one over
/// none of their speech), their longest segment as context; empty without
/// one. A leading "…" marks text that starts before the clip; text over
/// [`EXCERPT_MAX_LENGTH`] is cut with "…".
#[must_use]
pub fn excerpt(speaker: &Speaker, segments: &[TranscriptSegment]) -> Excerpt {
    let own: Vec<&TranscriptSegment> = segments
        .iter()
        .filter(|segment| segment.speaker_id == Some(speaker.id))
        .collect();
    if own.is_empty() {
        return Excerpt::default();
    }
    let mut used: Vec<&TranscriptSegment> = Vec::new();
    let mut cut_at_start = false;
    let mut in_clip = false;
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
        in_clip = used.iter().all(|segment| {
            segment.start >= range.lower - CLIP_EDGE && segment.end <= range.upper + CLIP_EDGE
        });
    }
    if used.is_empty() {
        let longest = own
            .iter()
            .copied()
            .max_by(|left, right| left.duration().total_cmp(&right.duration()))
            .expect("own is not empty");
        used = vec![longest];
        cut_at_start = false;
        in_clip = false;
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
    Excerpt { text, in_clip }
}

/// One speaker as the rows show it.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeakerRow {
    pub speaker: Speaker,
    /// The confirmed or suggested person, when the export has them.
    pub person: Option<Person>,
    /// What the speaker says in or around the sample clip ([`excerpt`]).
    pub excerpt: Excerpt,
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

    /// How strongly a `suggested` voice matches its person.
    #[must_use]
    pub fn suggestion_strength(&self) -> Option<VoiceStrength> {
        self.speaker
            .assignment
            .similarity()
            .and_then(VoiceStrength::of)
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

/// Makes `person` [`Settings::self_person_id`](steno_core::Settings::self_person_id)
/// unless a stored person is set, after confirming them as every
/// still-unknown "me" speaker ([`Store::confirm_unnamed_me_speakers`])
/// without exporting again, so a failed backfill is retried at the next
/// naming.
pub fn adopt_self_person(person: &Person, store: &Store) -> steno_core::store::Result<()> {
    if let Some(id) = store.settings()?.self_person_id
        && store.person(id)?.is_some()
    {
        return Ok(());
    }
    store.confirm_unnamed_me_speakers(person)?;
    update_settings(store, |settings| settings.self_person_id = Some(person.id))?;
    Ok(())
}

#[must_use]
pub fn clip_path(speaker: &Speaker) -> Option<PathBuf> {
    speaker.sample_clip_url.as_deref().and_then(file_url_path)
}

/// Why [`SpeakersViewModel::select`] named nobody.
#[derive(Debug, thiserror::Error)]
pub enum SelectError {
    /// The person already owns `voice` in this meeting, and confirming
    /// would merge the two speakers; the pick did not ask to combine.
    #[error("{person} already speaks as {voice} in this meeting.")]
    WouldCombine { person: String, voice: String },
    #[error("Speaker could not be named: {0}")]
    Store(#[from] steno_core::StoreError),
}

/// The speaker [`Store::confirm_speaker`] would merge `speaker` into when
/// confirmed as `person_id`: another speaker of the meeting confirmed as
/// that person, the first by cluster label.
fn combined_voice(
    store: &Store,
    speaker: &Speaker,
    person_id: Uuid,
) -> steno_core::store::Result<Option<Speaker>> {
    Ok(store
        .speakers(speaker.meeting_id)?
        .into_iter()
        .filter(|other| {
            other.id != speaker.id && other.assignment == SpeakerAssignment::Confirmed { person_id }
        })
        .min_by(|left, right| {
            (&left.cluster_label, left.id).cmp(&(&right.cluster_label, right.id))
        }))
}

#[derive(Debug, Default)]
pub struct SpeakersViewModel {
    pub rows: Vec<SpeakerRow>,
    pub export: Option<MeetingExport>,
    pub recent: Vec<Person>,
    pub persons: Vec<Person>,
    pub suggestions: BTreeMap<Uuid, SpeakerNameSuggestion>,
    /// The persons the user said No to, per speaker.
    pub rejections: BTreeMap<Uuid, BTreeSet<Uuid>>,
    pub error: Option<String>,
}

/// A voice match below [`VoiceStrength::FLOOR`] is not shown: the speaker
/// reads as unknown.
fn shown(mut speaker: Speaker) -> Speaker {
    if speaker
        .assignment
        .similarity()
        .is_some_and(|similarity| VoiceStrength::of(similarity).is_none())
    {
        speaker.assignment = SpeakerAssignment::Unknown;
    }
    speaker
}

impl SpeakersViewModel {
    /// The voices still to name or check; a dismissed voice is not one.
    #[must_use]
    pub fn unconfirmed_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| !row.is_confirmed() && !row.speaker.dismissed)
            .count()
    }

    #[must_use]
    pub fn row(&self, id: Uuid) -> Option<&SpeakerRow> {
        self.rows.iter().find(|row| row.id() == id)
    }

    /// A new export: rows are rebuilt and the suggestions, the user's
    /// Noes, the recent people and the people list reloaded. A voice match
    /// below [`VoiceStrength::FLOOR`] reads as unknown everywhere.
    pub fn update(&mut self, mut export: MeetingExport, store: &Store, files: &dyn FileSystem) {
        export.speakers = export.speakers.into_iter().map(shown).collect();
        self.rows = export
            .speakers
            .iter()
            .map(|speaker| SpeakerRow {
                person: speaker
                    .person_id()
                    .and_then(|id| export.persons.iter().find(|person| person.id == id))
                    .cloned(),
                excerpt: excerpt(speaker, &export.segments),
                can_play: clip_path(speaker).is_some_and(|path| files.exists(&path)),
                speaker: speaker.clone(),
            })
            .collect();
        let meeting_id = export.meeting.id;
        self.export = Some(export);
        let loaded = (|| -> steno_core::store::Result<()> {
            let suggestions = store.name_suggestions(meeting_id)?;
            let rejections = store.suggestion_rejections(meeting_id)?;
            let recent = store.recent_persons()?;
            let persons = store.persons()?;
            self.suggestions = suggestions
                .into_iter()
                .map(|suggestion| (suggestion.speaker_id, suggestion))
                .collect();
            self.rejections = rejections;
            self.recent = recent;
            self.persons = persons;
            Ok(())
        })();
        if let Err(error) = loaded {
            self.error = Some(format!("People could not be loaded: {error}"));
        }
    }

    /// The suggested person's name for a `suggested` speaker, which the
    /// field opens with.
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
        let none = BTreeSet::new();
        build_options(
            speaker,
            &OptionSources {
                speakers: &export.speakers,
                persons: &known,
                participants: &export.participants,
                suggestion,
                recent: &self.recent,
                rejected: self.rejections.get(&speaker_id).unwrap_or(&none),
            },
            query,
        )
    }

    /// The one write: a person option confirms that person; a create option
    /// resolves the name first, taking a same-named attendee's email. A
    /// person who owns another speaker here is merged into only with
    /// `combine`, else refused as [`SelectError::WouldCombine`]. The
    /// speaker's own person again changes nothing; an unlisted speaker is
    /// ignored. The first person the user names a "me" speaker as becomes the
    /// user's own ([`adopt_self_person`]). Returns whether a speaker changed.
    pub fn select(
        &mut self,
        option: &SpeakerOption,
        speaker_id: Uuid,
        combine: bool,
        store: &Store,
        now: DateTime<Utc>,
    ) -> Result<bool, SelectError> {
        let Some(row) = self.row(speaker_id).cloned() else {
            return Ok(false);
        };
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
        if !combine && let Some(owner) = combined_voice(store, &row.speaker, person.id)? {
            return Err(SelectError::WouldCombine {
                person: person.display_name,
                voice: owner.cluster_label,
            });
        }
        let clips = store.confirm_speaker(speaker_id, &person)?;
        for clip in clips.iter().filter_map(|clip| file_url_path(clip)) {
            let _ = std::fs::remove_file(clip);
        }
        if speaker_id == me_speaker_id(row.speaker.meeting_id)
            && let Err(error) = adopt_self_person(&person, store)
        {
            self.error = Some(format!("Steno could not remember who you are: {error}"));
        }
        Ok(true)
    }

    /// No to the suggestion that the speaker is `person_id`
    /// ([`Store::reject_suggestion`]): that person is not suggested for this
    /// voice again and nobody's voice changes. An unlisted speaker is
    /// ignored. Returns whether a speaker changed.
    pub fn reject(
        &self,
        speaker_id: Uuid,
        person_id: Uuid,
        store: &Store,
    ) -> steno_core::store::Result<bool> {
        if self.row(speaker_id).is_none() {
            return Ok(false);
        }
        store.reject_suggestion(speaker_id, person_id)?;
        Ok(true)
    }

    /// Remove name ([`Store::clear_speaker`]): the voice is unnamed again in
    /// this meeting; for "me" the meeting's "me" participant too, while who
    /// the user is stays. An unlisted or unnamed speaker is ignored.
    /// Returns whether a speaker changed.
    pub fn clear(&self, speaker_id: Uuid, store: &Store) -> steno_core::store::Result<bool> {
        let Some(row) = self.row(speaker_id) else {
            return Ok(false);
        };
        if row.speaker.assignment == SpeakerAssignment::Unknown {
            return Ok(false);
        }
        store.clear_speaker(speaker_id)?;
        Ok(true)
    }

    /// Not a person, or a person again ([`Store::set_speaker_dismissed`]).
    /// An unlisted speaker, or one already in that state, is ignored.
    /// Returns whether a speaker changed.
    pub fn set_dismissed(
        &self,
        speaker_id: Uuid,
        dismissed: bool,
        store: &Store,
    ) -> steno_core::store::Result<bool> {
        match self.row(speaker_id) {
            Some(row) if row.speaker.dismissed != dismissed => {
                store.set_speaker_dismissed(speaker_id, dismissed)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

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
