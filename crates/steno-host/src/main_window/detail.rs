//! One meeting's detail, after `Main/MeetingDetailViewModel.swift`: the
//! export and deliveries from the store, the bundled templates, the selected
//! tab and the speakers. Speaker changes apply at once and mark the meeting
//! dirty; the vault re-exports when the picker closes or the view goes away.

use chrono::{DateTime, Utc};
use steno_bridge::DetailTab;
use steno_core::protocols::{BoundaryResult, BoxError};
use steno_core::{
    AudioRetention, Delivery, Meeting, MeetingExport, MeetingOperation, MeetingStateKind, Platform,
    Settings, Store, StoreError, SummaryTemplate, paths::file_url_path, results_need_the_audio,
};
use uuid::Uuid;

use crate::services::{ClipPlayer, FileSystem, Pipeline, ProcessAgainRefusal};
use crate::setup::{ExportStatus, SummaryStatus, all_delivered, llm_configured, vault_configured};
use crate::speakers::SpeakersViewModel;

/// What the header's "Recording" line says; `None` for the one case
/// Settings already covers: kept forever with the files present.
/// Swift: `MeetingDetailViewModel.RecordingStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingStatus {
    /// The master file is gone (swept, or removed by hand).
    Deleted,
    /// `expiresAt` is set; the sweep removes the files from that day on.
    DeletesOn(DateTime<Utc>),
    /// A finite rule, no stamp, meeting ready, a delivery not delivered.
    KeptUntilExportSucceeds,
    /// A finite rule, no stamp, meeting failed: re-processing needs the audio.
    KeptProcessingFailed,
    /// A finite rule, no stamp, meeting ready and delivered, but its
    /// speakers or transcript may be incomplete
    /// ([`steno_core::results_need_the_audio`]), so the automatic retention
    /// keeps the recording. Rust only.
    KeptIncomplete,
    /// A finite rule, no stamp, meeting still on its way to the retention stage.
    KeptWhileProcessing,
}

/// Swift: `MeetingDetailViewModel`.
#[derive(Debug)]
pub struct MeetingDetailViewModel {
    pub id: Uuid,
    pub export: Option<MeetingExport>,
    pub deliveries: Vec<Delivery>,
    /// Whether the master file is on disk, rechecked with every export.
    pub recording_files_exist: bool,
    /// `Settings::default_retention`, followed through the settings.
    pub default_retention: AudioRetention,
    pub llm_configured: bool,
    pub vault_configured: bool,
    /// The launch stopped re-exporting the meeting
    /// ([`Pipeline::export_keeps_failing`]), read with the deliveries.
    pub export_keeps_failing: bool,
    pub error: Option<String>,
    pub is_busy: bool,
    pub tab: DetailTab,
    pub speakers: SpeakersViewModel,
    /// A speaker was named or reassigned since the last re-export.
    pub speakers_dirty: bool,
    /// The last re-export was refused because the pipeline held the
    /// meeting; try again when the export shows it ready.
    retry_redeliver_when_ready: bool,
}

impl MeetingDetailViewModel {
    /// Seeded from the stored settings so the rows and the footer render
    /// the configured state on their first frame.
    #[must_use]
    pub fn new(meeting_id: Uuid, settings: Option<&Settings>) -> Self {
        MeetingDetailViewModel {
            id: meeting_id,
            export: None,
            deliveries: Vec::new(),
            recording_files_exist: false,
            default_retention: settings.map_or(AudioRetention::KeepForever, |settings| {
                settings.default_retention
            }),
            llm_configured: settings.is_some_and(llm_configured),
            vault_configured: settings.is_some_and(vault_configured),
            export_keeps_failing: false,
            error: None,
            is_busy: false,
            tab: DetailTab::Summary,
            speakers: SpeakersViewModel::default(),
            speakers_dirty: false,
            retry_redeliver_when_ready: false,
        }
    }

    /// The bundled templates the picker offers.
    #[must_use]
    pub fn templates() -> &'static [SummaryTemplate] {
        SummaryTemplate::bundled()
    }

    /// The export and the deliveries as the store has them now: what the
    /// two store observations delivered. Feeds the speakers and retries a
    /// refused re-export once the meeting is ready.
    pub fn reload(&mut self, store: &Store, files: &dyn FileSystem, pipeline: &dyn Pipeline) {
        // A meeting deleted under the detail has no export, not an error.
        let export = match store.export(self.id) {
            Ok(export) => Ok(Some(export)),
            Err(StoreError::MeetingNotFound(_)) => Ok(None),
            Err(error) => Err(error),
        };
        match export {
            Ok(export) => {
                self.recording_files_exist = export
                    .as_ref()
                    .and_then(|export| export.audio.as_ref())
                    .and_then(|asset| file_url_path(&asset.url))
                    .is_some_and(|path| files.exists(&path));
                self.export = export;
                if let Some(export) = self.export.clone() {
                    self.speakers.update(export, store, files);
                    if self.retry_redeliver_when_ready
                        && self
                            .meeting()
                            .is_some_and(|meeting| meeting.state.kind() == MeetingStateKind::Ready)
                    {
                        self.retry_redeliver_when_ready = false;
                        self.redeliver_speaker_changes(pipeline);
                    }
                }
            }
            Err(error) => self.error = Some(format!("Meeting could not be loaded: {error}")),
        }
        match store.deliveries(self.id) {
            Ok(deliveries) => self.deliveries = deliveries,
            Err(error) => self.error = Some(format!("Deliveries could not be loaded: {error}")),
        }
        self.export_keeps_failing = pipeline.export_keeps_failing(self.id);
    }

    /// The settings as stored now: the keep toggle appears and disappears
    /// with the default rule, the rows and the footer with the endpoint and
    /// the vault.
    pub fn apply_settings(&mut self, settings: &Settings) {
        self.default_retention = settings.default_retention;
        self.llm_configured = llm_configured(settings);
        self.vault_configured = vault_configured(settings);
    }

    #[must_use]
    pub fn meeting(&self) -> Option<&Meeting> {
        self.export.as_ref().map(|export| &export.meeting)
    }

    /// The pipeline accepts a re-run or a re-export: ready or failed.
    #[must_use]
    pub fn can_rerun(&self) -> bool {
        self.meeting().is_some_and(|meeting| {
            matches!(
                meeting.state.kind(),
                MeetingStateKind::Ready | MeetingStateKind::Failed
            )
        })
    }

    /// A transcript to summarise: a meeting that failed before
    /// transcription must not be re-run.
    #[must_use]
    pub fn has_transcript(&self) -> bool {
        self.export
            .as_ref()
            .is_some_and(|export| !export.segments.is_empty())
    }

    /// "Re-run summary", "Run summary" and the failed row's "Try again".
    #[must_use]
    pub fn can_rerun_summary(&self) -> bool {
        self.can_rerun() && self.llm_configured && self.has_transcript()
    }

    /// "Process again": the meeting is one it is offered for
    /// ([`Meeting::offers_process_again`]) and its recording is on disk.
    /// The snapshot's `canProcessAgain`, and the guard of
    /// [`process_again`](Self::process_again), so the button and the action
    /// cannot drift apart.
    #[must_use]
    pub fn can_process_again(&self) -> bool {
        self.process_again_refusal().is_none()
    }

    /// Why the detail itself refuses "Process again": the meeting is not
    /// one it is offered for, or its recording is gone.
    fn process_again_refusal(&self) -> Option<ProcessAgainRefusal> {
        if !self.meeting().is_some_and(Meeting::offers_process_again) {
            Some(ProcessAgainRefusal::NotOffered)
        } else if !self.recording_files_exist {
            Some(ProcessAgainRefusal::RecordingGone)
        } else {
            None
        }
    }

    /// "Re-export" and "Export now": without a vault there is nowhere to
    /// export to.
    #[must_use]
    pub fn can_reexport(&self) -> bool {
        self.can_rerun() && self.vault_configured
    }

    #[must_use]
    pub fn summary_status(&self) -> SummaryStatus {
        SummaryStatus::of(self.meeting(), self.llm_configured)
    }

    #[must_use]
    pub fn export_status(&self) -> ExportStatus {
        ExportStatus::of(&self.deliveries, self.vault_configured)
    }

    /// The confirmed or suggested person's name, else the cluster label;
    /// "Unknown" without a speaker.
    #[must_use]
    pub fn display_name(&self, speaker_id: Option<Uuid>) -> String {
        match (speaker_id, &self.export) {
            (Some(id), Some(export)) => export.display_name_for_speaker(id),
            _ => "Unknown".to_owned(),
        }
    }

    // Actions

    /// Trimmed, lowercased, de-duplicated and sorted; empties dropped.
    /// Swift: `MeetingDetailViewModel.tags(from:)`.
    #[must_use]
    pub fn tags_from(list: &[String]) -> Vec<String> {
        let mut tags: Vec<String> = list
            .iter()
            .map(|tag| tag.trim().to_lowercase())
            .filter(|tag| !tag.is_empty())
            .collect();
        tags.sort();
        tags.dedup();
        tags
    }

    /// Tags as typed, comma separated.
    #[must_use]
    pub fn tags_from_text(text: &str) -> Vec<String> {
        Self::tags_from(&text.split(',').map(str::to_owned).collect::<Vec<_>>())
    }

    pub fn set_tags(&mut self, tags: Vec<String>, store: &Store, now: DateTime<Utc>) {
        self.update("Tags", store, now, move |meeting| meeting.tags = tags);
    }

    /// Stores the template and re-runs summarize plus deliver with it. An
    /// id the bundle does not have changes nothing.
    pub fn set_template(
        &mut self,
        template_id: &str,
        store: &Store,
        now: DateTime<Utc>,
        pipeline: &dyn Pipeline,
    ) {
        if SummaryTemplate::bundled_with_id(template_id).is_none() {
            return;
        }
        let id = template_id.to_owned();
        self.update("Template", store, now, move |meeting| {
            meeting.template_id = id;
        });
        self.rerun_summary_with(template_id, pipeline);
    }

    pub fn rerun_summary(&mut self, pipeline: &dyn Pipeline) {
        let Some(template_id) = self.meeting().map(|meeting| meeting.template_id.clone()) else {
            return;
        };
        self.rerun_summary_with(&template_id, pipeline);
    }

    fn rerun_summary_with(&mut self, template_id: &str, pipeline: &dyn Pipeline) {
        let id = self.id;
        self.run("Summary re-run", || pipeline.rerun_summary(id, template_id));
    }

    /// The only re-export entry point.
    pub fn reexport(&mut self, pipeline: &dyn Pipeline) {
        let id = self.id;
        self.run("Re-export", || pipeline.redeliver(id));
    }

    /// "Process again", offered while [`can_process_again`] holds: the
    /// pipeline saves the meeting queued and runs it from the start with
    /// its recording; the caller's reload then shows it queued. A refusal,
    /// the detail's own (not offered, or the recording gone) or the
    /// pipeline's, is the error line in [`process_again_refusal_line`]'s
    /// words; one while the app quits shows nothing. Rust only: the Swift
    /// app refuses it.
    ///
    /// [`can_process_again`]: Self::can_process_again
    pub fn process_again(&mut self, pipeline: &dyn Pipeline, platform: Platform) {
        let outcome = if let Some(refusal) = self.process_again_refusal() {
            Err(refusal)
        } else {
            self.is_busy = true;
            let outcome = pipeline.process_again(self.id);
            self.is_busy = false;
            outcome
        };
        match outcome {
            Ok(()) => self.error = None,
            Err(refusal) => {
                if let Some(line) = process_again_refusal_line(&refusal, platform) {
                    self.error = Some(line);
                }
            }
        }
    }

    // Recording line

    #[must_use]
    pub fn recording_status(&self) -> Option<RecordingStatus> {
        let export = self.export.as_ref()?;
        let asset = export.audio.as_ref()?;
        if !self.recording_files_exist {
            return Some(RecordingStatus::Deleted);
        }
        if asset.retention == AudioRetention::KeepForever {
            return None;
        }
        if let Some(expires_at) = asset.expires_at {
            return Some(RecordingStatus::DeletesOn(expires_at));
        }
        Some(match export.meeting.state.kind() {
            MeetingStateKind::Ready => {
                if !all_delivered(&self.deliveries) {
                    RecordingStatus::KeptUntilExportSucceeds
                } else if results_need_the_audio(
                    &export.meeting,
                    &export.speakers,
                    &export.segments,
                    asset,
                ) {
                    RecordingStatus::KeptIncomplete
                } else {
                    RecordingStatus::KeptWhileProcessing
                }
            }
            MeetingStateKind::Failed => RecordingStatus::KeptProcessingFailed,
            MeetingStateKind::Recording
            | MeetingStateKind::Queued
            | MeetingStateKind::Processing => RecordingStatus::KeptWhileProcessing,
        })
    }

    /// The per-meeting keep is offered only when the default does not keep
    /// everything already and there is a file to keep.
    #[must_use]
    pub fn shows_keep_toggle(&self) -> bool {
        self.default_retention != AudioRetention::KeepForever
            && self.recording_files_exist
            && self
                .export
                .as_ref()
                .is_some_and(|export| export.audio.is_some())
    }

    #[must_use]
    pub fn keeps_audio(&self) -> bool {
        self.export
            .as_ref()
            .and_then(|export| export.audio.as_ref())
            .is_some_and(|asset| asset.retention == AudioRetention::KeepForever)
    }

    /// The pipeline's stamp guard over the observed state: the default rule
    /// stamps today, the meeting is ready and nothing is left to export.
    #[must_use]
    pub fn would_delete_now(&self) -> bool {
        self.default_retention == AudioRetention::DeleteAfterProcessing
            && self
                .meeting()
                .is_some_and(|meeting| meeting.state.kind() == MeetingStateKind::Ready)
            && all_delivered(&self.deliveries)
    }

    // Speakers

    /// A speaker changed: the owner marks the meeting dirty.
    pub fn speakers_changed(&mut self) {
        self.speakers_dirty = true;
    }

    /// The header popover or a transcript picker closed: re-export once
    /// when a speaker changed, else nothing. A refusal keeps the flag and
    /// retries at the next ready tick.
    pub fn picker_closed(&mut self, pipeline: &dyn Pipeline) {
        if self.speakers_dirty {
            self.redeliver_speaker_changes(pipeline);
        }
    }

    fn redeliver_speaker_changes(&mut self, pipeline: &dyn Pipeline) {
        match pipeline.redeliver(self.id) {
            Ok(()) => self.speakers_dirty = false,
            Err(_) => self.retry_redeliver_when_ready = true,
        }
    }

    /// The view is going away (selection change, window closed): playback
    /// stops and a pending re-export is flushed once. Swift retried after
    /// three seconds in a detached task; the blocking host makes the one
    /// attempt.
    pub fn view_disappeared(&mut self, pipeline: &dyn Pipeline, player: &dyn ClipPlayer) {
        self.speakers.stop_playback(player);
        if !self.speakers_dirty {
            return;
        }
        self.speakers_dirty = false;
        self.retry_redeliver_when_ready = false;
        let _ = pipeline.redeliver(self.id);
    }

    fn update(
        &mut self,
        what: &str,
        store: &Store,
        now: DateTime<Utc>,
        mutate: impl FnOnce(&mut Meeting),
    ) {
        if let Err(error) = store.update_meeting(self.id, now, |meeting| {
            mutate(meeting);
            Ok(())
        }) {
            self.error = Some(format!("{what} could not be saved: {error}"));
        }
    }

    fn run(&mut self, what: &str, operation: impl FnOnce() -> BoundaryResult<()>) {
        self.is_busy = true;
        match operation() {
            Ok(()) => self.error = None,
            Err(error) => self.error = Some(format!("{what} failed: {error}")),
        }
        self.is_busy = false;
    }

    /// A re-run or a re-export the pipeline accepted failed later, in the
    /// background (`MeetingEvent::OperationFailed`): the error line says so
    /// in the words a refused call gets there, as Swift's awaited
    /// call did for both.
    pub fn operation_failed(&mut self, operation: MeetingOperation, failure: &str) {
        self.error = Some(format!("{} failed: {failure}", operation.label()));
    }
}

/// What the detail's error line says when "Process again" is refused;
/// `None` while the app quits, which the user asked for.
#[must_use]
pub fn process_again_refusal_line(
    refusal: &ProcessAgainRefusal,
    platform: Platform,
) -> Option<String> {
    Some(match refusal {
        ProcessAgainRefusal::MeetingGone => "This meeting no longer exists.".to_owned(),
        ProcessAgainRefusal::NotOffered => {
            "Only a failed meeting can be processed again.".to_owned()
        }
        ProcessAgainRefusal::RecordingGone => platform
            .mac_or(
                "The recording is no longer on this Mac, so the meeting cannot be processed again.",
                "The recording is no longer on this computer, so the meeting cannot be processed again.",
            )
            .to_owned(),
        ProcessAgainRefusal::Busy => "This meeting is already being processed.".to_owned(),
        ProcessAgainRefusal::Quitting => return None,
        ProcessAgainRefusal::CouldNotStart(reason) => {
            format!("Processing could not start: {reason}")
        }
    })
}

/// The keep flag on `meeting_id`, selected or not: `keep` sets
/// `keepForever`; off restores the default retention from Settings. Both go
/// through the pipeline's `apply_retention`, so the stamp and the
/// `retentionApplied` post are the pipeline's, and so is the one refusal (a
/// meeting without an audio asset); a processing meeting takes the rule and
/// is stamped later. The error is the line the detail shows. Swift:
/// `MeetingDetailViewModel.setKeepAudio(_:)`.
pub fn set_keep_audio(
    meeting_id: Uuid,
    keep: bool,
    store: &Store,
    pipeline: &dyn Pipeline,
) -> Result<(), String> {
    let rule = if keep {
        Ok(AudioRetention::KeepForever)
    } else {
        store
            .settings()
            .map(|settings| settings.default_retention)
            .map_err(BoxError::from)
    };
    rule.and_then(|rule| pipeline.apply_retention(meeting_id, rule))
        .map_err(|error| format!("Retention could not be changed: {error}"))
}
