//! The two recording intakes: the phone's (`RecordingIntake`) and the
//! Mac's (`LocalRecordingIntake`). Every rule about the rows lives here so
//! the app, the CLI and the handover service share one spelling.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, FixedOffset, Utc};
use steno_core::{
    AdmissionCommit, AudioAsset, AudioFormat, AudioLane, AudioRetention, HandoverIntake,
    HandoverReceipt, HandoverState, Meeting, MeetingSource, MeetingState, MeetingStateKind,
    PairedDevice, Participant, ParticipantRole, RecordingEndReason, RecordingLayout,
    RecordingMetadata, Store, StoreError, TitleOrigin, async_trait, derived_uuid, paths::file_url,
    paths::file_url_path, protocols::BoundaryResult,
};
use uuid::Uuid;

use crate::pipeline::{Now, PipelineFailure, ProcessingPipeline};

/// Hands a meeting and asset the phone intake saved to the pipeline
/// ([`ProcessingPipeline::enqueue_saved`] in production).
pub type Enqueue = Arc<
    dyn Fn(Meeting, AudioAsset) -> Pin<Box<dyn Future<Output = Result<(), PipelineFailure>> + Send>>
        + Send
        + Sync,
>;

/// Commits the stopped Mac recording (meeting id, result, asset retention,
/// `None` for the settings' default) and hands it to the pipeline
/// ([`ProcessingPipeline::enqueue_stopped_recording`] in production);
/// returns the meeting as saved.
pub type EnqueueStopped = Arc<
    dyn Fn(
            Uuid,
            RecordingResult,
            Option<AudioRetention>,
        ) -> Pin<Box<dyn Future<Output = Result<Meeting, PipelineFailure>> + Send>>
        + Send
        + Sync,
>;

/// "Phone recording 2026-09-24 11:00" in the given zone.
#[must_use]
pub fn phone_title(started_at: DateTime<Utc>, zone: FixedOffset) -> String {
    default_title(MeetingSource::Phone, started_at, zone)
}

/// "Call 2026-09-24 11:00", "Meeting 2026-09-24 11:00" or "Phone recording
/// 2026-09-24 11:00" in `zone`.
#[must_use]
pub fn default_title(
    source: MeetingSource,
    started_at: DateTime<Utc>,
    zone: FixedOffset,
) -> String {
    let kind = match source {
        MeetingSource::MacCall => "Call",
        MeetingSource::MacInPerson => "Meeting",
        MeetingSource::Phone => "Phone recording",
    };
    format!(
        "{kind} {}",
        started_at.with_timezone(&zone).format("%Y-%m-%d %H:%M")
    )
}

/// Where the phone intake notes the meeting folder it copies an upload
/// into, from before the copy until the admission commits or the copy is
/// gone. A copy left with no meeting is then known to be this install's, so
/// the launch adopts it and the phone's retry admits into it
/// ([`AdmissionFolders::noted_copy`]). `()` notes nothing.
pub trait AdmissionFolders: Send + Sync {
    /// Before the copy of `upload` into `meeting_id`'s folder. An error
    /// refuses the admission before anything is copied or committed.
    fn copying(
        &self,
        meeting_id: Uuid,
        audio_folder: &Path,
        upload: &NotedUpload,
    ) -> std::io::Result<()>;
    /// The admission of `meeting_id` committed, or its copy is gone.
    fn settled(&self, meeting_id: Uuid);
    /// The meeting id and audio folder of an unsettled noted copy of
    /// `upload`, if any.
    fn noted_copy(&self, _upload: &NotedUpload) -> Option<(Uuid, std::path::PathBuf)> {
        None
    }
}

impl AdmissionFolders for () {
    fn copying(&self, _: Uuid, _: &Path, _: &NotedUpload) -> std::io::Result<()> {
        Ok(())
    }

    fn settled(&self, _: Uuid) {}
}

/// The upload a phone admission's copy holds, as the admission ledger keys
/// it: the recording id, the size and the SHA-256.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotedUpload {
    pub recording_id: Uuid,
    pub byte_count: i64,
    pub sha256: Vec<u8>,
}

impl NotedUpload {
    /// The upload `metadata` announces.
    #[must_use]
    pub fn of(metadata: &RecordingMetadata) -> Self {
        NotedUpload {
            recording_id: metadata.recording_id,
            byte_count: metadata.byte_count,
            sha256: metadata.sha256.clone(),
        }
    }

    /// Whether the file at `path` is this upload whole (size and SHA-256);
    /// a copy cut short by a crash is not.
    pub fn is_held_by(&self, path: &Path) -> std::io::Result<bool> {
        let len = std::fs::metadata(path)?.len();
        if i64::try_from(len).ok() != Some(self.byte_count) {
            return Ok(false);
        }
        Ok(steno_core::content_hash::sha256_of_file(path)? == self.sha256)
    }
}

/// Admits a fully received phone recording: copies the file into the audio
/// folder, commits the `HandoverReceipt` as complete together with a
/// `queued` phone meeting and its asset, deletes the upload, and only then
/// hands the meeting to the pipeline. Idempotent on `recording_id`.
///
/// The phone deletes its copy once `complete` answers 200, so the copy and
/// its folders are synced and the receipt, meeting and asset commit in one
/// durable transaction first. When that commit fails, the receipt is saved
/// `failed` durably and only then is the copy removed: a failed commit can
/// still be replayed after a crash, and its meeting then needs the copy.
/// Bytes the admission ledger already holds with a meeting are that
/// meeting: the receipt completes with it and nothing is enqueued. The
/// admission runs in a task of its own, so a phone that hangs up mid-admit
/// leaves it to finish.
pub struct RecordingIntake {
    store: Arc<Store>,
    enqueue: Enqueue,
    now: Now,
    zone: FixedOffset,
    folders: Arc<dyn AdmissionFolders>,
}

impl RecordingIntake {
    /// How many times the `failed` receipt save after a failed commit is
    /// tried while another connection holds the database; each try already
    /// waits out the store's five-second busy timeout.
    pub const FAILED_SAVE_ATTEMPTS: usize = 3;

    /// How long the intake waits, with the writer free, before each try of
    /// the `failed` save, so a queued write of this process gets the writer
    /// meanwhile.
    pub const FAILED_SAVE_BACK_OFF: Duration = Duration::from_millis(200);

    #[must_use]
    pub fn new(store: Arc<Store>, enqueue: Enqueue, now: Now, zone: FixedOffset) -> Self {
        RecordingIntake {
            store,
            enqueue,
            now,
            zone,
            folders: Arc::new(()),
        }
    }

    /// The intake noting each copy's folder in `folders`.
    #[must_use]
    pub fn noting_folders_in(mut self, folders: Arc<dyn AdmissionFolders>) -> Self {
        self.folders = folders;
        self
    }

    /// The production wiring over `pipeline`.
    #[must_use]
    pub fn over(store: Arc<Store>, pipeline: ProcessingPipeline, zone: FixedOffset) -> Self {
        let now = pipeline.dependencies().now.clone();
        Self::new(
            store,
            Arc::new(move |meeting, asset| {
                let pipeline = pipeline.clone();
                Box::pin(async move { pipeline.enqueue_saved(&meeting, &asset) })
            }),
            now,
            zone,
        )
    }
}

/// What the intake's one store step decided ([`admit_on`]).
enum Admission {
    /// Answered from the store or the ledger: nothing to enqueue.
    Answered(Uuid),
    /// Committed: the meeting and its asset to hand to the pipeline.
    Admitted(Box<(Meeting, AudioAsset)>),
}

#[async_trait]
impl HandoverIntake for RecordingIntake {
    /// The store step runs the reads, the copy, the commit and the `failed`
    /// save; a panic in the task resumes here.
    async fn admit(
        &self,
        file: &Path,
        metadata: &RecordingMetadata,
        device: &PairedDevice,
    ) -> BoundaryResult<Uuid> {
        let step = AdmitStep {
            file: file.to_path_buf(),
            metadata: metadata.clone(),
            device: device.clone(),
            now: self.now.clone(),
            zone: self.zone,
            folders: self.folders.clone(),
        };
        let admitted = tokio::spawn(admit_and_enqueue(
            self.store.clone(),
            self.enqueue.clone(),
            step,
        ));
        match admitted.await {
            Ok(outcome) => outcome,
            Err(join) => match join.try_into_panic() {
                Ok(panic) => std::panic::resume_unwind(panic),
                Err(cancelled) => Err(cancelled.into()),
            },
        }
    }
}

/// [`RecordingIntake::admit`]'s task: the store step, then the enqueue.
async fn admit_and_enqueue(
    store: Arc<Store>,
    enqueue: Enqueue,
    step: AdmitStep,
) -> BoundaryResult<Uuid> {
    let admission = store
        .writing(move |store| Ok(admit_on(store, &step)))
        .await??;
    match admission {
        Admission::Answered(meeting_id) => Ok(meeting_id),
        Admission::Admitted(admitted) => {
            let (meeting, asset) = *admitted;
            let meeting_id = meeting.id;
            // Admitted: the meeting stays `queued` for the next launch.
            if let Err(failure) = enqueue(meeting, asset).await {
                tracing::warn!(%meeting_id, stage = failure.stage.as_str(), "admitted meeting not enqueued");
            }
            Ok(meeting_id)
        }
    }
}

/// What [`admit_on`] needs, owned, so the step runs on the blocking pool.
struct AdmitStep {
    file: std::path::PathBuf,
    metadata: RecordingMetadata,
    device: PairedDevice,
    now: Now,
    zone: FixedOffset,
    folders: Arc<dyn AdmissionFolders>,
}

/// [`RecordingIntake::admit`] up to the enqueue, on `store`.
#[allow(
    clippy::too_many_lines,
    reason = "one admission, in the order it commits"
)]
fn admit_on(store: &Store, step: &AdmitStep) -> BoundaryResult<Admission> {
    let AdmitStep {
        file,
        metadata,
        device,
        now,
        zone,
        folders,
    } = step;
    let existing = store.handover_receipt(metadata.recording_id)?;
    // Another upload's receipt under this id (another phone's, or other
    // bytes under the same id) is never completed or answered from: its
    // `complete` would delete a file never admitted.
    if existing.as_ref().is_some_and(|receipt| {
        receipt.device_id != device.id
            || receipt.byte_count != metadata.byte_count
            || receipt.sha256 != metadata.sha256
    }) {
        return Err(StoreError::ReceiptOfAnotherUpload(metadata.recording_id).into());
    }
    // A retry of an admitted recording is answered from the store without
    // writing: every earlier commit is already durable.
    if let Some(meeting_id) = existing
        .as_ref()
        .and_then(|receipt| receipt.state.meeting_id())
        && store.meeting(meeting_id)?.is_some()
    {
        return Ok(Admission::Answered(meeting_id));
    }

    let settings = store.settings()?;
    let timestamp = now();
    let upload = NotedUpload::of(metadata);
    // A second copy would be a second meeting once the launch adopts the
    // first.
    let reused = reusable_copy(store, folders.as_ref(), &upload, metadata.format)?;
    let is_reused = reused.is_some();
    let (meeting_id, layout, destination) = if let Some(reused) = reused {
        reused
    } else {
        let meeting_id = Uuid::new_v4();
        let audio_folder = file_url_path(&settings.audio_folder)
            .ok_or_else(|| format!("audio folder is not a file URL: {}", settings.audio_folder))?;
        let layout = RecordingLayout::new(&audio_folder, meeting_id);
        // Durable before the receipt says complete: the phone deletes its
        // copy on that answer.
        folders.copying(meeting_id, &audio_folder, &upload)?;
        let destination = copy_into_new_folder(file, &layout, metadata.format)
            .inspect_err(|_| folders.settled(meeting_id))?;
        (meeting_id, layout, destination)
    };

    let meeting = Meeting {
        id: meeting_id,
        title: phone_title(metadata.started_at, *zone),
        started_at: metadata.started_at,
        duration: metadata.duration_seconds,
        language: None,
        source: MeetingSource::Phone,
        calendar_event_id: None,
        tags: Vec::new(),
        state: MeetingState::Queued,
        end_reason: None,
        title_origin: TitleOrigin::Default,
        template_id: settings.default_template_id.clone(),
        summary: None,
        scratchpad: String::new(),
        llm_usage: None,
        created_at: timestamp,
        updated_at: timestamp,
    };
    let asset = AudioAsset {
        id: Uuid::new_v4(),
        meeting_id,
        url: file_url(&destination, false),
        format: metadata.format,
        lanes: vec![AudioLane::Mixed],
        sidecars_16k: std::collections::BTreeMap::new(),
        mixdown_url: None,
        retention: settings.default_retention,
        expires_at: None,
    };
    let mut receipt = existing.unwrap_or_else(|| HandoverReceipt {
        recording_id: metadata.recording_id,
        device_id: device.id,
        state: HandoverState::Receiving,
        byte_count: metadata.byte_count,
        sha256: metadata.sha256.clone(),
        chunk_size: metadata.chunk_size,
        received_chunks: Vec::new(),
        created_at: timestamp,
        updated_at: timestamp,
    });
    receipt.state = HandoverState::Complete { meeting_id };
    receipt.updated_at = timestamp;

    let committed = match store.commit_admission_durably(&receipt, &meeting, &asset) {
        Ok(committed) => {
            // The noted copy stays noted: the launch decides its fate.
            let answered_elsewhere = is_reused
                && matches!(committed, AdmissionCommit::Earlier { meeting_id: admitted, .. } if admitted != meeting_id);
            if !answered_elsewhere {
                folders.settled(meeting_id);
            }
            committed
        }
        Err(error) => {
            // The refusal of another upload's receipt wrote nothing. Any
            // other failed commit may still be replayed from the WAL after a
            // crash, so the copy goes only once the durable `failed` save
            // writes over it; otherwise it stays noted for the next launch.
            let copy_goes = matches!(error, StoreError::ReceiptOfAnotherUpload(_)) || {
                receipt.state = HandoverState::Failed(format!("admit: {error}"));
                save_failed_receipt(store, &receipt)
            };
            if copy_goes && !is_reused {
                let _ = std::fs::remove_file(&destination);
                folders.settled(meeting_id);
            }
            return Err(error.into());
        }
    };
    let _ = std::fs::remove_file(file);
    match committed {
        AdmissionCommit::Wrote => Ok(Admission::Admitted(Box::new((meeting, asset)))),
        AdmissionCommit::Earlier {
            meeting_id: admitted,
            ..
        } => {
            // The ledger's meeting owns these bytes: a new copy belongs to
            // no meeting, and the noted copy is left alone.
            if !is_reused {
                let _ = std::fs::remove_file(&destination);
                let _ = std::fs::remove_dir(&layout.directory);
            }
            Ok(Admission::Answered(admitted))
        }
    }
}

/// The copy of `upload` an earlier admission noted and neither committed
/// nor removed, when its meeting has no row and the copy still holds the
/// upload whole: its meeting id, layout and master in `format`. Otherwise
/// the upload is copied again.
fn reusable_copy(
    store: &Store,
    folders: &dyn AdmissionFolders,
    upload: &NotedUpload,
    format: AudioFormat,
) -> BoundaryResult<Option<(Uuid, RecordingLayout, std::path::PathBuf)>> {
    let Some((meeting_id, audio_folder)) = folders.noted_copy(upload) else {
        return Ok(None);
    };
    if store.meeting(meeting_id)?.is_some() {
        return Ok(None);
    }
    #[cfg(feature = "testing")]
    seams::after_the_reuse_check(meeting_id);
    let layout = RecordingLayout::new(&audio_folder, meeting_id);
    let master = layout.master(format);
    match upload.is_held_by(&master) {
        Ok(true) => Ok(Some((meeting_id, layout, master))),
        held => {
            // The error can name the user's folder: debug alone.
            tracing::debug!(%meeting_id, ?held, "a noted copy of the upload is not admitted into");
            Ok(None)
        }
    }
}

/// The phone intake's test seam: race tests hold a retry between its
/// no-row check for the noted copy and its commit. Behind `testing`.
#[cfg(feature = "testing")]
pub mod seams {
    use std::sync::{Arc, Mutex};

    use uuid::Uuid;

    type Hook = Arc<dyn Fn(Uuid) + Send + Sync>;

    static AFTER_THE_REUSE_CHECK: Mutex<Vec<Hook>> = Mutex::new(Vec::new());

    /// Calls `hook` with the noted copy's meeting id each time a retry found
    /// it has no row. Hooks stay for the process.
    pub fn on_the_reuse_check(hook: impl Fn(Uuid) + Send + Sync + 'static) {
        AFTER_THE_REUSE_CHECK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Arc::new(hook));
    }

    pub(super) fn after_the_reuse_check(meeting_id: Uuid) {
        let hooks = AFTER_THE_REUSE_CHECK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        for hook in hooks {
            hook(meeting_id);
        }
    }
}

/// The durable `failed` save of `receipt` after a failed admission commit,
/// tried up to [`RecordingIntake::FAILED_SAVE_ATTEMPTS`] times while the
/// database is busy, backing off holding nothing: whether it is on disk.
fn save_failed_receipt(store: &Store, receipt: &HandoverReceipt) -> bool {
    for attempt in 1..=RecordingIntake::FAILED_SAVE_ATTEMPTS {
        std::thread::sleep(RecordingIntake::FAILED_SAVE_BACK_OFF);
        match store.save_handover_receipt_durably(receipt) {
            Err(error) if error.is_busy() => {
                tracing::debug!(attempt, "the failed receipt's save found the database busy");
            }
            saved => return saved.is_ok(),
        }
    }
    false
}

/// Copies the verified upload `file` durably into `layout`'s new meeting
/// folder and returns the copy's path. An existing folder at that path
/// fails before the copy; a failed copy removes the folder it made.
fn copy_into_new_folder(
    file: &Path,
    layout: &RecordingLayout,
    format: AudioFormat,
) -> std::io::Result<std::path::PathBuf> {
    crate::files::create_new_dir_durably(&layout.directory)?;
    let destination = layout.master(format);
    crate::files::copy_durably(file, &destination).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&layout.directory);
    })?;
    Ok(destination)
}

/// What a finished capture hands to [`LocalRecordingIntake::complete`].
#[derive(Debug, Clone, PartialEq)]
pub struct RecordingResult {
    pub asset: AudioAsset,
    pub duration: f64,
    pub end_reason: RecordingEndReason,
}

/// A calendar attendee other than the user; becomes a `them` participant.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Attendee {
    pub display_name: String,
    pub email: Option<String>,
}

/// Errors of the Mac recording transaction beyond the store's own.
#[derive(Debug, thiserror::Error)]
pub enum LocalRecordingIntakeError {
    /// `complete` on a meeting that is not `recording`: nothing is written.
    #[error("meeting {0} is {1}, not recording")]
    NotRecording(Uuid, MeetingStateKind),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Pipeline(#[from] PipelineFailure),
}

impl LocalRecordingIntakeError {
    /// Another connection held the database past the busy timeout.
    #[must_use]
    pub fn is_busy(&self) -> bool {
        match self {
            LocalRecordingIntakeError::Store(error) => error.is_busy(),
            LocalRecordingIntakeError::Pipeline(failure) => failure.is_busy(),
            LocalRecordingIntakeError::NotRecording(..) => false,
        }
    }
}

/// The Mac recording transaction: `begin` writes the `recording` row,
/// `complete` turns the capture into a `queued` meeting for the pipeline,
/// `fail` records why a recording never made it.
pub struct LocalRecordingIntake {
    store: Arc<Store>,
    enqueue: EnqueueStopped,
    now: Now,
    zone: FixedOffset,
    keep_trying: Box<dyn Fn() -> bool + Send + Sync>,
}

impl LocalRecordingIntake {
    #[must_use]
    pub fn new(store: Arc<Store>, enqueue: EnqueueStopped, now: Now, zone: FixedOffset) -> Self {
        LocalRecordingIntake {
            store,
            enqueue,
            now,
            zone,
            keep_trying: Box::new(|| true),
        }
    }

    /// This intake with [`Self::complete`] asking `keep_trying` before each
    /// further try of a busy commit, so a stop during the app's exit ends in
    /// time.
    #[must_use]
    pub fn retrying_while(
        mut self,
        keep_trying: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        self.keep_trying = Box::new(keep_trying);
        self
    }

    /// The production wiring over `pipeline`.
    #[must_use]
    pub fn over(store: Arc<Store>, pipeline: ProcessingPipeline, zone: FixedOffset) -> Self {
        let now = pipeline.dependencies().now.clone();
        Self::new(
            store,
            Arc::new(move |meeting_id, result, retention| {
                let pipeline = pipeline.clone();
                Box::pin(async move {
                    pipeline.enqueue_stopped_recording(meeting_id, &result, retention)
                })
            }),
            now,
            zone,
        )
    }

    /// Writes the `recording` meeting `meeting_id` and its `them`
    /// participants in one transaction. An empty title becomes the default
    /// title; a title with a calendar event id is `calendar`, one without
    /// is `user`.
    pub fn begin(
        &self,
        meeting_id: Uuid,
        source: MeetingSource,
        title: Option<&str>,
        calendar_event_id: Option<&str>,
        attendees: &[Attendee],
        started_at: DateTime<Utc>,
    ) -> Result<Meeting, LocalRecordingIntakeError> {
        let settings = self.store.settings()?;
        let timestamp = (self.now)();
        let trimmed = title.map(str::trim).unwrap_or_default();
        let title_origin = if trimmed.is_empty() {
            TitleOrigin::Default
        } else if calendar_event_id.is_none() {
            TitleOrigin::User
        } else {
            TitleOrigin::Calendar
        };
        let meeting = Meeting {
            id: meeting_id,
            title: if trimmed.is_empty() {
                default_title(source, started_at, self.zone)
            } else {
                trimmed.to_owned()
            },
            started_at,
            duration: 0.0,
            language: None,
            source,
            calendar_event_id: calendar_event_id.map(str::to_owned),
            tags: Vec::new(),
            state: MeetingState::Recording,
            end_reason: None,
            title_origin,
            template_id: settings.default_template_id,
            summary: None,
            scratchpad: String::new(),
            llm_usage: None,
            created_at: timestamp,
            updated_at: timestamp,
        };
        let participants = participants_from(attendees, meeting.id);
        self.store
            .save_meeting_with_participants(&meeting, &participants)?;
        Ok(meeting)
    }

    /// Names the recording `meeting_id`, begun without an event, after the
    /// calendar event that came in since, as [`Self::begin`] would have
    /// ([`Store::name_recording_after_event`]); returns whether it wrote.
    pub fn name_after_event(
        &self,
        meeting_id: Uuid,
        title: &str,
        calendar_event_id: &str,
        attendees: &[Attendee],
    ) -> Result<bool, LocalRecordingIntakeError> {
        let trimmed = title.trim();
        let participants = participants_from(attendees, meeting_id);
        Ok(self.store.name_recording_after_event(
            meeting_id,
            (!trimmed.is_empty()).then_some(trimmed),
            calendar_event_id,
            &participants,
            (self.now)(),
        )?)
    }

    /// How many times [`Self::complete`] tries its commit while another
    /// connection holds the database.
    pub const COMMIT_ATTEMPTS: usize = 3;

    /// Writes the duration, the end reason and the asset's retention, and
    /// enqueues the meeting: `queued` meeting and asset in one commit, which
    /// writes only while the row is still `recording` and keeps what else
    /// was saved on it since ([`Store::save_stopped_recording`]).
    ///
    /// A busy commit is tried again, up to [`Self::COMMIT_ATTEMPTS`] in
    /// all, while [`Self::retrying_while`]'s check allows. A meeting no
    /// longer `recording` is left alone. Any other failure leaves it
    /// `recording` and returns the error; the next launch's recovery queues
    /// it.
    pub async fn complete(
        &self,
        meeting_id: Uuid,
        result: RecordingResult,
        retention: Option<AudioRetention>,
    ) -> Result<Meeting, LocalRecordingIntakeError> {
        let mut attempt = 1;
        loop {
            match self
                .complete_inner(meeting_id, result.clone(), retention)
                .await
            {
                Err(error)
                    if error.is_busy()
                        && attempt < Self::COMMIT_ATTEMPTS
                        && (self.keep_trying)() =>
                {
                    tracing::debug!(%meeting_id, attempt, "the recording's commit found the database busy");
                    attempt += 1;
                }
                outcome => return outcome,
            }
        }
    }

    async fn complete_inner(
        &self,
        meeting_id: Uuid,
        result: RecordingResult,
        retention: Option<AudioRetention>,
    ) -> Result<Meeting, LocalRecordingIntakeError> {
        let failure = match (self.enqueue)(meeting_id, result, retention).await {
            Ok(meeting) => return Ok(meeting),
            Err(failure) => failure,
        };
        // Only a non-busy failure is the row's to record, and only while
        // it is still this recording.
        if !failure.is_busy()
            && let Ok(now) = self.store.meeting(meeting_id)
        {
            match now {
                None => return Err(StoreError::MeetingNotFound(meeting_id).into()),
                Some(now) if now.state != MeetingState::Recording => {
                    return Err(LocalRecordingIntakeError::NotRecording(
                        meeting_id,
                        now.state.kind(),
                    ));
                }
                Some(_) => {}
            }
        }
        Err(failure.into())
    }

    /// Marks the meeting failed: a capture that could not start or be saved.
    pub fn fail(&self, meeting_id: Uuid, reason: &str) -> Result<(), LocalRecordingIntakeError> {
        self.store.set_state(
            meeting_id,
            MeetingState::Failed {
                reason: reason.to_owned(),
            },
            (self.now)(),
        )?;
        Ok(())
    }
}

/// One `them` participant per distinct attendee (by email, else by name,
/// case-insensitively), ids derived from the meeting id so a retried
/// `begin` writes the same rows.
#[must_use]
pub fn participants_from(attendees: &[Attendee], meeting_id: Uuid) -> Vec<Participant> {
    let mut seen = std::collections::BTreeSet::new();
    let mut participants = Vec::new();
    for attendee in attendees {
        let name = attendee.display_name.trim();
        if name.is_empty() {
            continue;
        }
        let email = attendee
            .email
            .as_deref()
            .map(|email| email.trim().to_lowercase())
            .filter(|email| !email.is_empty());
        let key = email.as_ref().map_or_else(
            || format!("name:{}", name.to_lowercase()),
            |email| format!("email:{email}"),
        );
        if !seen.insert(key) {
            continue;
        }
        participants.push(Participant {
            id: derived_uuid(meeting_id, &format!("attendee-{}", participants.len())),
            meeting_id,
            person_id: None,
            display_name: name.to_owned(),
            role: ParticipantRole::Them,
            email,
        });
    }
    participants
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use steno_core::PipelineStage;
    use steno_core::testing::WriteLockHold;

    use super::*;

    /// Every file under `root`, recursively.
    fn files_under(root: &Path) -> Vec<std::path::PathBuf> {
        let mut files = Vec::new();
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    files.extend(files_under(&path));
                } else {
                    files.push(path);
                }
            }
        }
        files
    }

    /// A store under `dir` whose audio folder is `dir/audio`.
    fn store_with_audio_folder(dir: &Path) -> Arc<Store> {
        let store = Arc::new(Store::open(dir.join("steno.sqlite")).unwrap());
        let mut settings = store.settings().unwrap();
        settings.audio_folder = file_url(&dir.join("audio"), true);
        store.save_settings(&settings).unwrap();
        store
    }

    /// A phone paired in `store` and a nine-byte upload's metadata from it.
    fn paired_phone(store: &Store, now: DateTime<Utc>) -> (PairedDevice, RecordingMetadata) {
        let device = PairedDevice {
            id: Uuid::new_v4(),
            name: "Phone".to_owned(),
            paired_at: now,
            last_seen_at: None,
        };
        store.save_paired_device(&device, &[1; 32]).unwrap();
        let metadata = RecordingMetadata {
            recording_id: Uuid::new_v4(),
            started_at: now,
            duration_seconds: 12.0,
            byte_count: 9,
            sha256: vec![0; 32],
            chunk_size: 9,
            format: AudioFormat::M4aAac,
            device_name: "Phone".to_owned(),
        };
        (device, metadata)
    }

    /// The phone intake over `store` with `enqueue` and `now`, in UTC.
    fn phone_intake(store: &Arc<Store>, enqueue: Enqueue, now: Now) -> RecordingIntake {
        RecordingIntake::new(
            store.clone(),
            enqueue,
            now,
            FixedOffset::east_opt(0).unwrap(),
        )
    }

    /// The nine-byte upload [`paired_phone`]'s metadata describes, in `dir`.
    fn upload_in(dir: &Path) -> std::path::PathBuf {
        let upload = dir.join("upload.m4a");
        std::fs::write(&upload, b"aac bytes").unwrap();
        upload
    }

    /// `device`'s receipt of `metadata`'s recording in `state`.
    fn receipt_of(
        device: &PairedDevice,
        metadata: &RecordingMetadata,
        state: HandoverState,
        now: DateTime<Utc>,
    ) -> HandoverReceipt {
        HandoverReceipt {
            recording_id: metadata.recording_id,
            device_id: device.id,
            state,
            byte_count: metadata.byte_count,
            sha256: metadata.sha256.clone(),
            chunk_size: metadata.chunk_size,
            received_chunks: vec![0],
            created_at: now,
            updated_at: now,
        }
    }

    /// A Mac recording `begin` wrote, and what its capture handed back.
    fn a_recording(store: &Arc<Store>, dir: &Path) -> (Meeting, RecordingResult) {
        let at = DateTime::parse_from_rfc3339("2026-09-24T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let intake = LocalRecordingIntake::new(
            store.clone(),
            Arc::new(|_, _, _| {
                Box::pin(async { Err(PipelineFailure::new(PipelineStage::Decode, "unused")) })
            }),
            Arc::new(move || at),
            FixedOffset::east_opt(0).unwrap(),
        );
        let meeting = intake
            .begin(
                Uuid::new_v4(),
                MeetingSource::MacInPerson,
                None,
                None,
                &[],
                at,
            )
            .unwrap();
        assert_eq!(
            store.meeting(meeting.id).unwrap().unwrap().state,
            MeetingState::Recording
        );
        let result = RecordingResult {
            asset: AudioAsset {
                id: Uuid::new_v4(),
                meeting_id: meeting.id,
                url: file_url(&dir.join("recording.caf"), false),
                format: AudioFormat::Caf48kFloat32,
                lanes: vec![AudioLane::Mixed],
                sidecars_16k: std::collections::BTreeMap::new(),
                retention: AudioRetention::KeepForever,
                expires_at: None,
                mixdown_url: None,
            },
            duration: 12.0,
            end_reason: RecordingEndReason::Manual,
        };
        (meeting, result)
    }

    /// An enqueue that commits as the pipeline's does and counts its calls;
    /// `after_call` runs after each.
    fn committing_enqueue(
        store: Arc<Store>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
        after_call: impl Fn(usize) + Send + Sync + 'static,
    ) -> EnqueueStopped {
        let after_call = Arc::new(after_call);
        Arc::new(move |meeting_id, result, retention| {
            let (store, calls, after_call) = (store.clone(), calls.clone(), after_call.clone());
            Box::pin(async move {
                let committed = commit_stopped(&store, meeting_id, &result, retention);
                after_call(calls.fetch_add(1, Ordering::SeqCst) + 1);
                committed
            })
        })
    }

    /// The pipeline's commit of a stopped recording.
    fn commit_stopped(
        store: &Store,
        meeting_id: Uuid,
        result: &RecordingResult,
        retention: Option<AudioRetention>,
    ) -> Result<Meeting, PipelineFailure> {
        store
            .save_stopped_recording(
                meeting_id,
                result.duration,
                result.end_reason.clone(),
                &result.asset,
                retention,
                Utc::now(),
            )
            .map_err(|error| PipelineFailure::wrapping(&error, PipelineStage::Decode))
    }

    /// A busy commit is tried again and lands; one that stays busy leaves
    /// the meeting `recording` and returns the busy error.
    #[tokio::test]
    async fn a_busy_commit_is_tried_again_and_one_that_stays_busy_leaves_the_recording() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("steno.sqlite");
        let store = Arc::new(Store::open(&path).unwrap());
        store
            .on_writer(|connection| connection.busy_timeout(Duration::from_millis(20)))
            .unwrap();

        let (meeting, result) = a_recording(&store, dir.path());
        let hold = Mutex::new(Some(WriteLockHold::new(&path)));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let enqueue = committing_enqueue(store.clone(), calls.clone(), move |_| {
            // The first commit found the lock; the next one will not.
            hold.lock().unwrap().take();
        });
        let intake = LocalRecordingIntake::new(
            store.clone(),
            enqueue,
            Arc::new(Utc::now),
            FixedOffset::east_opt(0).unwrap(),
        );
        let completed = intake
            .complete(meeting.id, result.clone(), None)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2, "tried twice");
        assert_eq!(completed.state, MeetingState::Queued);
        assert_eq!(
            store.meeting(meeting.id).unwrap().unwrap().state,
            MeetingState::Queued
        );
        assert!(store.asset(meeting.id).unwrap().is_some());

        // Released after the last try, so a later write would land and show.
        let (meeting, result) = a_recording(&store, dir.path());
        let hold = Mutex::new(Some(WriteLockHold::new(&path)));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let enqueue = committing_enqueue(store.clone(), calls.clone(), move |call| {
            if call == LocalRecordingIntake::COMMIT_ATTEMPTS {
                hold.lock().unwrap().take();
            }
        });
        let intake = LocalRecordingIntake::new(
            store.clone(),
            enqueue,
            Arc::new(Utc::now),
            FixedOffset::east_opt(0).unwrap(),
        );
        let error = intake.complete(meeting.id, result, None).await.unwrap_err();
        assert!(error.is_busy(), "{error}");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            LocalRecordingIntake::COMMIT_ATTEMPTS
        );
        let kept = store.meeting(meeting.id).unwrap().unwrap();
        assert_eq!(kept.state, MeetingState::Recording, "not failed");
        assert_eq!(kept, meeting, "the row is as `begin` wrote it");
        assert!(store.asset(meeting.id).unwrap().is_none());
    }

    /// Makes every meeting insert fail; with `failed_receipts_too`, the
    /// `failed` receipt save as well.
    fn refuse_writes(store: &Store, failed_receipts_too: bool) {
        let mut sql = String::from(
            "CREATE TRIGGER refuse_meetings BEFORE INSERT ON meeting \
             BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
        );
        if failed_receipts_too {
            for event in ["INSERT", "UPDATE"] {
                write!(
                    sql,
                    "CREATE TRIGGER refuse_failed_{event} BEFORE {event} ON handoverReceipt \
                     WHEN NEW.state = 'failed' BEGIN SELECT RAISE(ABORT, 'disk full'); END;"
                )
                .unwrap();
            }
        }
        store
            .write(|transaction| Ok(transaction.execute_batch(&sql)?))
            .unwrap();
    }

    /// What [`recording_enqueue`] was handed, in order.
    type Enqueued = Arc<Mutex<Vec<(Meeting, AudioAsset)>>>;

    /// An enqueue that records what it was handed.
    fn recording_enqueue() -> (Enqueue, Enqueued) {
        let admitted: Enqueued = Arc::default();
        let seen = admitted.clone();
        let enqueue: Enqueue = Arc::new(move |meeting, asset| {
            seen.lock().unwrap().push((meeting, asset));
            Box::pin(async { Ok(()) })
        });
        (enqueue, admitted)
    }

    /// A phone recording becomes its meeting's master and the upload goes;
    /// a refused one leaves a `failed` receipt, the upload and no copy.
    #[tokio::test]
    async fn a_phone_recording_lands_in_the_audio_folder_and_a_refused_one_leaves_no_copy() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_audio_folder(dir.path());
        let audio = dir.path().join("audio");
        let now = DateTime::parse_from_rfc3339("2026-09-24T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let (enqueue, admitted) = recording_enqueue();
        let intake = phone_intake(&store, enqueue, Arc::new(move || now));
        let (device, metadata) = paired_phone(&store, now);
        let upload = upload_in(dir.path());

        let meeting_id = intake.admit(&upload, &metadata, &device).await.unwrap();
        let (meeting, asset) = admitted.lock().unwrap()[0].clone();
        assert_eq!(meeting.id, meeting_id);
        assert_eq!(meeting.title, "Phone recording 2026-09-24 09:00");
        assert_eq!(store.meeting(meeting_id).unwrap(), Some(meeting));
        assert_eq!(store.asset(meeting_id).unwrap(), Some(asset.clone()));
        let master = RecordingLayout::new(&audio, meeting_id).master(AudioFormat::M4aAac);
        assert!(master.starts_with(&audio));
        assert_eq!(asset.url, file_url(&master, false));
        assert_eq!(asset.lanes, vec![AudioLane::Mixed]);
        assert_eq!(std::fs::read(&master).unwrap(), b"aac bytes");
        assert!(!upload.exists(), "the upload is deleted once admitted");
        assert!(matches!(
            store.handover_receipt(metadata.recording_id).unwrap().unwrap().state,
            HandoverState::Complete { meeting_id: id } if id == meeting_id
        ));
        assert_eq!(
            store
                .admitted_meeting(metadata.recording_id, metadata.byte_count, &metadata.sha256)
                .unwrap(),
            Some(meeting_id),
            "the admission's ledger row commits with it"
        );

        refuse_writes(&store, false);
        std::fs::write(&upload, b"aac bytes").unwrap();
        let second = RecordingMetadata {
            recording_id: Uuid::new_v4(),
            ..metadata.clone()
        };
        let error = intake.admit(&upload, &second, &device).await.unwrap_err();
        assert!(error.to_string().contains("disk full"), "{error}");
        assert_eq!(
            files_under(&audio),
            vec![master.clone()],
            "the refused recording's copy is gone"
        );
        assert!(upload.exists(), "a refused upload is kept for the retry");
        assert!(matches!(
            store
                .handover_receipt(second.recording_id)
                .unwrap()
                .unwrap()
                .state,
            HandoverState::Failed(_)
        ));
        assert_eq!(store.all_meetings().unwrap().len(), 1);
        assert_eq!(admitted.lock().unwrap().len(), 1, "nothing is enqueued");
    }

    /// [`AdmissionFolders`] that lists each call; `copying` fails while
    /// `refuse` is set.
    #[derive(Default)]
    struct NotedFolders {
        calls: Mutex<Vec<(Uuid, Option<usize>)>>,
        refuse: AtomicBool,
    }

    impl AdmissionFolders for NotedFolders {
        fn copying(
            &self,
            meeting_id: Uuid,
            audio_folder: &Path,
            _: &NotedUpload,
        ) -> std::io::Result<()> {
            if self.refuse.load(Ordering::SeqCst) {
                return Err(std::io::Error::other("read-only"));
            }
            let files = files_under(audio_folder).len();
            self.calls.lock().unwrap().push((meeting_id, Some(files)));
            Ok(())
        }

        fn settled(&self, meeting_id: Uuid) {
            self.calls.lock().unwrap().push((meeting_id, None));
        }
    }

    /// The copy's folder is noted before the copy and settled on commit; a
    /// note that cannot be written refuses the admission.
    #[tokio::test]
    async fn a_copys_folder_is_noted_from_before_the_copy_until_its_admission_settles() {
        for (refuse_note, refuse_commit) in [(false, false), (false, true), (true, false)] {
            let dir = tempfile::tempdir().unwrap();
            let store = store_with_audio_folder(dir.path());
            let now = Utc::now();
            let (enqueue, _) = recording_enqueue();
            let folders = Arc::new(NotedFolders::default());
            folders.refuse.store(refuse_note, Ordering::SeqCst);
            let intake = phone_intake(&store, enqueue, Arc::new(move || now))
                .noting_folders_in(folders.clone());
            let (device, metadata) = paired_phone(&store, now);
            let upload = upload_in(dir.path());
            if refuse_commit {
                refuse_writes(&store, true);
            }

            let admitted = intake.admit(&upload, &metadata, &device).await;
            let calls = folders.calls.lock().unwrap().clone();
            let copies = files_under(&dir.path().join("audio"));
            match (refuse_note, refuse_commit) {
                (false, false) => {
                    let meeting_id = admitted.unwrap();
                    assert_eq!(calls, [(meeting_id, Some(0)), (meeting_id, None)]);
                }
                (false, true) => {
                    admitted.unwrap_err();
                    assert!(matches!(calls[..], [(_, Some(0))]), "{calls:?}");
                    assert_eq!(copies.len(), 1, "the noted copy stays");
                }
                _ => {
                    admitted.unwrap_err();
                    assert!(calls.is_empty(), "{calls:?}");
                    assert!(copies.is_empty(), "nothing is copied");
                    assert!(upload.exists(), "the upload stays for the retry");
                    assert_eq!(store.all_meetings().unwrap(), []);
                }
            }
        }
    }

    /// An enqueue that fails after the commit still admits; the meeting
    /// stays `queued`.
    #[tokio::test]
    async fn an_enqueue_that_fails_after_the_commit_still_admits() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_audio_folder(dir.path());
        let now = Utc::now();
        let enqueue: Enqueue = Arc::new(|_, _| {
            Box::pin(async { Err(PipelineFailure::new(PipelineStage::Decode, "no runtime")) })
        });
        let intake = phone_intake(&store, enqueue, Arc::new(move || now));
        let (device, metadata) = paired_phone(&store, now);
        let upload = upload_in(dir.path());

        let meeting_id = intake.admit(&upload, &metadata, &device).await.unwrap();

        assert_eq!(
            store.meeting(meeting_id).unwrap().unwrap().state,
            MeetingState::Queued
        );
        assert!(store.asset(meeting_id).unwrap().is_some());
        assert_eq!(
            store
                .handover_receipt(metadata.recording_id)
                .unwrap()
                .unwrap()
                .state,
            HandoverState::Complete { meeting_id }
        );
        assert!(!upload.exists());
    }

    /// A failed copy admits nothing and leaves no meeting folder.
    #[tokio::test]
    async fn an_upload_that_cannot_be_copied_is_not_marked_complete() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_audio_folder(dir.path());
        let now = Utc::now();
        let enqueued = Arc::new(AtomicBool::new(false));
        let enqueue: Enqueue = {
            let enqueued = enqueued.clone();
            Arc::new(move |_, _| {
                enqueued.store(true, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            })
        };
        let intake = phone_intake(&store, enqueue, Arc::new(move || now));
        let (device, metadata) = paired_phone(&store, now);
        let missing = dir.path().join("gone.m4a");
        intake
            .admit(&missing, &metadata, &device)
            .await
            .expect_err("nothing to copy");
        let receipt = store.handover_receipt(metadata.recording_id).unwrap();
        assert!(
            !receipt.is_some_and(|receipt| matches!(receipt.state, HandoverState::Complete { .. })),
            "a receipt whose copy failed is not complete"
        );
        assert_eq!(store.all_meetings().unwrap(), []);
        assert!(!enqueued.load(Ordering::SeqCst));
        let folders: Vec<_> = std::fs::read_dir(dir.path().join("audio"))
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .collect();
        assert_eq!(folders, Vec::<std::path::PathBuf>::new());
    }

    /// A folder already at the new meeting's path fails the copy untouched.
    #[test]
    fn a_folder_already_at_the_meeting_path_is_never_written_or_removed() {
        let dir = tempfile::tempdir().unwrap();
        let upload = upload_in(dir.path());
        let layout = RecordingLayout::new(&dir.path().join("audio"), Uuid::new_v4());
        let earlier = layout.master(AudioFormat::M4aAac);
        std::fs::create_dir_all(&layout.directory).unwrap();
        std::fs::write(&earlier, b"an earlier recording").unwrap();
        let error = copy_into_new_folder(&upload, &layout, AudioFormat::M4aAac).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&earlier).unwrap(), b"an earlier recording");
        assert_eq!(files_under(&layout.directory), [earlier]);
        assert!(upload.exists());
    }

    /// A decoder and dispatcher nothing reaches.
    struct Unreached;

    #[async_trait]
    impl steno_core::AudioDecoder for Unreached {
        async fn decode(
            &self,
            _: &AudioAsset,
            _: AudioLane,
        ) -> BoundaryResult<steno_core::AudioBuffer16k> {
            unreachable!("nothing is processed")
        }

        fn mixdown_format(&self) -> AudioFormat {
            AudioFormat::Wav16kInt16
        }

        async fn mixdown(&self, _: &AudioAsset, _: &Path) -> BoundaryResult<()> {
            unreachable!("nothing is processed")
        }
    }

    #[async_trait]
    impl steno_core::DeliveryDispatcher for Unreached {
        async fn deliver_all(&self, _: Uuid) -> Vec<steno_core::Delivery> {
            unreachable!("nothing is processed")
        }
    }

    /// One write transaction as the commit probe saw it.
    #[derive(Debug, Clone, PartialEq)]
    struct Commit {
        /// `PRAGMA synchronous`: 1 is `NORMAL`, 2 is `FULL`.
        synchronous: i64,
        /// The recording's receipt state, if it has a receipt.
        receipt: Option<String>,
        /// Whether a phone meeting exists.
        meeting: bool,
    }

    /// Every commit on `store` from now on, about `recording_id`.
    fn commits_of(store: &Store, recording_id: Uuid) -> Arc<Mutex<Vec<Commit>>> {
        let commits: Arc<Mutex<Vec<Commit>>> = Arc::default();
        let seen = commits.clone();
        store.probe_commits(move |connection| {
            let commit = Commit {
                synchronous: connection
                    .query_row("PRAGMA synchronous", [], |row| row.get(0))
                    .unwrap(),
                receipt: connection
                    .query_row(
                        "SELECT (SELECT state FROM handoverReceipt WHERE recordingID = ?1)",
                        [steno_core::store::convert::DbUuid(recording_id)],
                        |row| row.get(0),
                    )
                    .unwrap(),
                meeting: connection
                    .query_row(
                        "SELECT count(*) > 0 FROM meeting WHERE source = 'phone'",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap(),
            };
            seen.lock().unwrap().push(commit);
        });
        commits
    }

    /// A pipeline over `store` nothing reaches, its clock stopped at `now`.
    fn unreached_pipeline(store: &Arc<Store>, now: DateTime<Utc>) -> ProcessingPipeline {
        ProcessingPipeline::new(
            crate::PipelineDependencies::new(
                Arc::new(Unreached),
                Arc::new(steno_core::testing::FakeSpeechEngine::default()),
                Arc::new(steno_core::testing::FakeDiarizer::default()),
                Arc::new(steno_core::testing::InMemorySpeakerMemory::new(Vec::new())),
                Arc::new(Unreached),
                store.clone(),
                crate::MeetingEventBus::new(),
            )
            .with_now(Arc::new(move || now)),
        )
    }

    /// The production local intake's enqueue refuses a row that failed
    /// meanwhile with `NotRecording` and writes nothing.
    #[tokio::test]
    async fn the_production_local_intake_commits_only_a_row_still_recording() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_audio_folder(dir.path());
        let (meeting, result) = a_recording(&store, dir.path());
        let failed = MeetingState::Failed { reason: "x".into() };
        store
            .set_state(meeting.id, failed.clone(), Utc::now())
            .unwrap();
        let intake = LocalRecordingIntake::over(
            store.clone(),
            unreached_pipeline(&store, Utc::now()),
            FixedOffset::east_opt(0).unwrap(),
        );
        let failure = (intake.enqueue)(meeting.id, result, None)
            .await
            .unwrap_err();
        assert_eq!(
            failure.reason,
            StoreError::NotRecording(meeting.id, MeetingStateKind::Failed).to_string()
        );
        assert_eq!(store.meeting(meeting.id).unwrap().unwrap().state, failed);
        assert!(store.asset(meeting.id).unwrap().is_none());
    }

    /// `PRAGMA synchronous` on the store's writer outside a write.
    fn synchronous(store: &Store) -> i64 {
        store
            .on_writer(|connection| {
                connection.query_row("PRAGMA synchronous", [], |row| row.get(0))
            })
            .unwrap()
    }

    /// The production intake commits the `complete` receipt and the meeting
    /// in one `synchronous = FULL` transaction, and leaves `NORMAL` after.
    #[tokio::test]
    async fn the_production_intake_commits_its_receipt_and_meeting_durably() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_audio_folder(dir.path());
        let now = Utc::now();
        let pipeline = unreached_pipeline(&store, now);
        // The meeting is saved and stays queued; nothing is processed.
        pipeline.quit();
        let intake =
            RecordingIntake::over(store.clone(), pipeline, FixedOffset::east_opt(0).unwrap());
        let (device, metadata) = paired_phone(&store, now);
        let upload = upload_in(dir.path());
        assert_eq!(synchronous(&store), 1);
        let commits = commits_of(&store, metadata.recording_id);

        let meeting_id = intake.admit(&upload, &metadata, &device).await.unwrap();

        let commits = commits.lock().unwrap().clone();
        let admitted = Commit {
            synchronous: 2,
            receipt: Some("complete".to_owned()),
            meeting: true,
        };
        assert_eq!(
            commits,
            [admitted],
            "one commit holds the receipt and the meeting, under FULL; the \
             enqueue writes nothing"
        );
        assert_eq!(synchronous(&store), 1, "the connection is back at NORMAL");
        assert!(store.meeting(meeting_id).unwrap().is_some());
    }

    /// A `complete` receipt whose meeting is gone is admitted again into a
    /// new meeting.
    #[tokio::test]
    async fn a_complete_receipt_whose_meeting_is_gone_is_admitted_again() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_audio_folder(dir.path());
        let now = Utc::now();
        let (enqueue, admitted) = recording_enqueue();
        let intake = phone_intake(&store, enqueue, Arc::new(move || now));
        let (device, metadata) = paired_phone(&store, now);
        let missing = Uuid::new_v4();
        store
            .save_handover_receipt(&receipt_of(
                &device,
                &metadata,
                HandoverState::Complete {
                    meeting_id: missing,
                },
                now,
            ))
            .unwrap();
        let upload = upload_in(dir.path());

        let meeting_id = intake.admit(&upload, &metadata, &device).await.unwrap();

        assert_ne!(meeting_id, missing);
        assert!(store.meeting(meeting_id).unwrap().is_some());
        assert_eq!(admitted.lock().unwrap().len(), 1);
        assert_eq!(
            store
                .handover_receipt(metadata.recording_id)
                .unwrap()
                .unwrap()
                .state,
            HandoverState::Complete { meeting_id }
        );
        assert!(!upload.exists());
    }

    #[test]
    fn attendees_are_deduplicated_by_email_then_name() {
        let meeting = Uuid::new_v4();
        let attendees = vec![
            Attendee {
                display_name: " Anna ".into(),
                email: Some("Anna@Example.com".into()),
            },
            Attendee {
                display_name: "anna".into(),
                email: Some("anna@example.com".into()),
            },
            Attendee {
                display_name: "Ben".into(),
                email: None,
            },
            Attendee {
                display_name: "BEN".into(),
                email: Some(String::new()),
            },
            Attendee {
                display_name: "  ".into(),
                email: None,
            },
        ];
        let participants = participants_from(&attendees, meeting);
        assert_eq!(
            participants
                .iter()
                .map(|p| p.display_name.as_str())
                .collect::<Vec<_>>(),
            ["Anna", "Ben"]
        );
        assert_eq!(participants[0].email.as_deref(), Some("anna@example.com"));
        assert_eq!(participants[0].id, derived_uuid(meeting, "attendee-0"));
    }

    #[test]
    fn default_titles_follow_the_swift_wording() {
        let at = DateTime::parse_from_rfc3339("2026-09-24T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let berlin = FixedOffset::east_opt(2 * 3600).unwrap();
        assert_eq!(
            default_title(MeetingSource::MacCall, at, berlin),
            "Call 2026-09-24 11:00"
        );
        assert_eq!(
            default_title(MeetingSource::MacInPerson, at, berlin),
            "Meeting 2026-09-24 11:00"
        );
        assert_eq!(phone_title(at, berlin), "Phone recording 2026-09-24 11:00");
    }
}
