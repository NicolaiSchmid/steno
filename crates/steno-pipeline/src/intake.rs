//! The two recording intakes: the phone's (`RecordingIntake`, core's
//! `HandoverIntake`) and the Mac's (`LocalRecordingIntake`). Every rule
//! about the rows lives here so the app, the CLI and the handover service
//! share one spelling. Swift: `Sources/StenoCore/Storage/RecordingIntake.swift`
//! and `Sources/StenoCore/Storage/LocalRecordingIntake.swift`.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, FixedOffset, Utc};
use steno_core::{
    AudioAsset, AudioFormat, AudioLane, AudioRetention, HandoverIntake, HandoverReceipt,
    HandoverState, Meeting, MeetingSource, MeetingState, MeetingStateKind, PairedDevice,
    Participant, ParticipantRole, RecordingEndReason, RecordingLayout, RecordingMetadata, Store,
    StoreError, TitleOrigin, async_trait, derived_uuid, paths::file_url, paths::file_url_path,
    protocols::BoundaryResult,
};
use uuid::Uuid;

use crate::pipeline::{Now, PipelineFailure, ProcessingPipeline};

/// Hands a queued meeting and its asset to the pipeline. The production
/// wiring is [`ProcessingPipeline::enqueue`] for the local intake, which
/// saves the rows, and [`ProcessingPipeline::enqueue_saved`] for the
/// phone's, which saved them itself; tests pass a counting closure.
pub type Enqueue = Arc<
    dyn Fn(Meeting, AudioAsset) -> Pin<Box<dyn Future<Output = Result<(), PipelineFailure>> + Send>>
        + Send
        + Sync,
>;

/// `enqueue` (one of [`ProcessingPipeline`]'s enqueues) on `pipeline`.
fn enqueue_through(
    pipeline: ProcessingPipeline,
    enqueue: fn(&ProcessingPipeline, &Meeting, &AudioAsset) -> Result<(), PipelineFailure>,
) -> Enqueue {
    Arc::new(move |meeting, asset| {
        let pipeline = pipeline.clone();
        Box::pin(async move { enqueue(&pipeline, &meeting, &asset) })
    })
}

/// "Phone recording 2026-09-24 11:00" in the given zone.
#[must_use]
pub fn phone_title(started_at: DateTime<Utc>, zone: FixedOffset) -> String {
    default_title(MeetingSource::Phone, started_at, zone)
}

/// "Call 2026-09-24 11:00", "Meeting 2026-09-24 11:00" or "Phone recording
/// 2026-09-24 11:00" in `zone`. Swift: `LocalRecordingIntake.defaultTitle`.
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

/// Admits a fully received phone recording: copies the file into the
/// audio folder (`RecordingLayout`), commits the `HandoverReceipt` as
/// complete together with a `phone` meeting `queued` with a `mixed`
/// asset under the default retention, deletes the upload, and only then
/// hands the meeting to the pipeline. Idempotent on `recording_id`.
///
/// The phone deletes its own copy once `complete` answers 200, so
/// everything the admission wrote is on the disk first. The parent of
/// every folder it created, the copy and the meeting folder are synced
/// before the receipt is marked complete
/// ([`crate::files::create_new_dir_durably`],
/// [`crate::files::copy_durably`]); Swift syncs its `copyItem` copy and
/// the folders the same way. The receipt, the meeting and its asset
/// commit in one durable transaction ([`Store::save_admission_durably`]),
/// so no crash, full disk or busy store leaves a `complete` receipt
/// without its meeting. When that commit fails, the receipt is saved
/// `failed` durably ([`Store::save_handover_receipt_durably`]), and the
/// copy is removed only once that save succeeds: a failed commit can still
/// be replayed after a crash, and its meeting then needs the copy. The
/// phone keeps its own copy either way. Bytes the admission ledger already
/// holds with a meeting (another device's upload of them, taken over
/// during the first admission) are that meeting: the receipt is completed
/// with it, the copy goes, and nothing is enqueued.
/// The enqueue after the commit is [`ProcessingPipeline::enqueue_saved`]
/// in [`RecordingIntake::over`]; its failure does not undo the admission,
/// the meeting waits `queued` for the next launch's resume.
/// Swift: `Sources/StenoCore/Storage/RecordingIntake.swift`.
pub struct RecordingIntake {
    store: Arc<Store>,
    enqueue: Enqueue,
    now: Now,
    zone: FixedOffset,
}

impl RecordingIntake {
    #[must_use]
    pub fn new(store: Arc<Store>, enqueue: Enqueue, now: Now, zone: FixedOffset) -> Self {
        RecordingIntake {
            store,
            enqueue,
            now,
            zone,
        }
    }

    /// The production wiring over `pipeline`, which processes the
    /// meetings the intake saved ([`ProcessingPipeline::enqueue_saved`]).
    #[must_use]
    pub fn over(store: Arc<Store>, pipeline: ProcessingPipeline, zone: FixedOffset) -> Self {
        let now = pipeline.dependencies().now.clone();
        Self::new(
            store,
            enqueue_through(pipeline, ProcessingPipeline::enqueue_saved),
            now,
            zone,
        )
    }
}

#[async_trait]
impl HandoverIntake for RecordingIntake {
    async fn admit(
        &self,
        file: &Path,
        metadata: &RecordingMetadata,
        device: &PairedDevice,
    ) -> BoundaryResult<Uuid> {
        let existing = self.store.handover_receipt(metadata.recording_id)?;
        // Another upload's receipt under this id is never completed or
        // answered from: another phone's (this one was revoked, and that one
        // announced the id or took the receipt over), which would take this
        // meeting for its own and delete its copy; or one of other bytes (the
        // phone announced another file under the id), whose `complete` would
        // get this meeting and delete a file never admitted.
        if existing.as_ref().is_some_and(|receipt| {
            receipt.device_id != device.id
                || receipt.byte_count != metadata.byte_count
                || receipt.sha256 != metadata.sha256
        }) {
            return Err(StoreError::ReceiptOfAnotherUpload(metadata.recording_id).into());
        }
        // A retry of an admitted recording is answered from the store with
        // no write of its own: the launch checkpoint
        // (`HandoverService::checkpoint_store`) put every earlier commit on
        // the disk, and this process commits its admissions durably.
        if let Some(meeting_id) = existing
            .as_ref()
            .and_then(|receipt| receipt.state.meeting_id())
            && self.store.meeting(meeting_id)?.is_some()
        {
            return Ok(meeting_id);
        }

        let settings = self.store.settings()?;
        let meeting_id = Uuid::new_v4();
        let timestamp = (self.now)();
        let audio_folder = file_url_path(&settings.audio_folder)
            .ok_or_else(|| format!("audio folder is not a file URL: {}", settings.audio_folder))?;
        let layout = RecordingLayout::new(&audio_folder, meeting_id);
        // The copy and its folder are on the disk before the receipt says
        // complete: the phone deletes its own copy on that answer.
        let destination = copy_into_new_folder(file, &layout, metadata.format)?;

        let meeting = Meeting {
            id: meeting_id,
            title: phone_title(metadata.started_at, self.zone),
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

        let admitted = match self
            .store
            .save_admission_durably(&receipt, &meeting, &asset)
        {
            Ok(admitted) => admitted,
            Err(error) => {
                if matches!(error, StoreError::ReceiptOfAnotherUpload(_)) {
                    // The refusal wrote nothing, and another upload's
                    // receipt is left as it is.
                    let _ = std::fs::remove_file(&destination);
                } else {
                    // A failed commit is not proof that nothing committed: a
                    // WAL sync that fails leaves the commit's frames in the
                    // WAL, and recovery after a crash replays them. The
                    // durable `failed` save writes over them, or voids them
                    // when the WAL restarts, so the copy goes only once that
                    // save is on the disk; otherwise it stays, an orphan at
                    // worst, and a replayed admission still finds its master.
                    receipt.state = HandoverState::Failed(format!("admit: {error}"));
                    if self.store.save_handover_receipt_durably(&receipt).is_ok() {
                        let _ = std::fs::remove_file(&destination);
                    }
                }
                return Err(error.into());
            }
        };
        let _ = std::fs::remove_file(file);
        if admitted != meeting_id {
            // The ledger held these bytes with a meeting: the receipt is
            // complete with it, and this copy and its folder belong to no
            // meeting.
            let _ = std::fs::remove_file(&destination);
            let _ = std::fs::remove_dir(&layout.directory);
            return Ok(admitted);
        }
        // Admitted: a pipeline that cannot take the meeting now leaves it
        // `queued`, and the next launch resumes it.
        if let Err(failure) = (self.enqueue)(meeting, asset).await {
            tracing::warn!(%meeting_id, stage = failure.stage.as_str(), "admitted meeting not enqueued");
        }
        Ok(meeting_id)
    }
}

/// Copies the verified upload `file` durably into the new meeting folder of
/// `layout` and returns the copy's path. A folder already at the layout's
/// path fails the attempt before the copy and is left as it is
/// ([`crate::files::create_new_dir_durably`]). A failed copy removes the
/// meeting folder, which only this call made; the upload and the phone's
/// copy remain, and the phone's retry copies into a new folder.
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
/// Swift: `RecordingResult`.
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
    /// `complete` on a meeting that is not `recording`: nothing is written
    /// and nothing is marked failed.
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

/// The Mac recording transaction. `begin` writes the `recording` row the
/// list shows while the capture session runs; `complete` turns the
/// finished capture into a `queued` meeting with its asset and hands both
/// to the pipeline; `fail` records why a recording never made it.
pub struct LocalRecordingIntake {
    store: Arc<Store>,
    enqueue: Enqueue,
    now: Now,
    zone: FixedOffset,
    commit_attempts: usize,
}

impl LocalRecordingIntake {
    #[must_use]
    pub fn new(store: Arc<Store>, enqueue: Enqueue, now: Now, zone: FixedOffset) -> Self {
        LocalRecordingIntake {
            store,
            enqueue,
            now,
            zone,
            commit_attempts: Self::COMMIT_ATTEMPTS,
        }
    }

    /// This intake with [`Self::complete`] trying its commit `attempts`
    /// times in all (at least once) instead of [`Self::COMMIT_ATTEMPTS`]:
    /// a stop for the app's exit tries once, so it ends within the exit's
    /// patience.
    #[must_use]
    pub fn with_commit_attempts(mut self, attempts: usize) -> Self {
        self.commit_attempts = attempts.max(1);
        self
    }

    /// The production wiring over `pipeline`.
    #[must_use]
    pub fn over(store: Arc<Store>, pipeline: ProcessingPipeline, zone: FixedOffset) -> Self {
        let now = pipeline.dependencies().now.clone();
        Self::new(
            store,
            enqueue_through(pipeline, ProcessingPipeline::enqueue),
            now,
            zone,
        )
    }

    /// Writes the `recording` meeting and its `them` participants in one
    /// transaction. An empty title becomes the default title with
    /// `title_origin` default; a title with a calendar event id is
    /// `calendar`, one without is `user`.
    pub fn begin(
        &self,
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
            id: Uuid::new_v4(),
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

    /// How many times [`Self::complete`] tries its commit while another
    /// connection holds the database: each try already waits out the
    /// store's five-second busy timeout.
    pub const COMMIT_ATTEMPTS: usize = 3;

    /// Writes the duration and the end reason, sets the asset's retention
    /// (`retention`, else the settings' default as it is now) with
    /// `expires_at` cleared, and enqueues the meeting: the meeting, now
    /// `queued`, and its asset in one commit. A commit that finds the
    /// database busy is tried again, up to [`Self::COMMIT_ATTEMPTS`] in
    /// all ([`Self::with_commit_attempts`]). A meeting that is not
    /// `recording` is left alone, its row untouched. Any other failure
    /// leaves the meeting `recording` and returns the error: the recording
    /// stays on disk, and the next launch's recovery finds it there and
    /// queues it. Swift marked the meeting failed without its asset, so the
    /// recording was lost to the list; the retry and the recording kept are
    /// Rust only.
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
                Err(error) if error.is_busy() && attempt < self.commit_attempts => {
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
        let timestamp = (self.now)();
        let retention = match retention {
            Some(retention) => retention,
            None => self.store.settings()?.default_retention,
        };
        let current = self
            .store
            .meeting(meeting_id)?
            .ok_or(StoreError::MeetingNotFound(meeting_id))?;
        if current.state != MeetingState::Recording {
            return Err(LocalRecordingIntakeError::NotRecording(
                meeting_id,
                current.state.kind(),
            ));
        }
        // The enqueue writes the meeting and the asset in one commit, so a
        // failed one leaves the row as it was.
        let mut meeting = current;
        meeting.duration = result.duration;
        meeting.end_reason = Some(result.end_reason);
        meeting.updated_at = timestamp;
        meeting.state = MeetingState::Queued;
        let mut asset = result.asset;
        asset.meeting_id = meeting_id;
        asset.retention = retention;
        asset.expires_at = None;
        (self.enqueue)(meeting.clone(), asset).await?;
        Ok(meeting)
    }

    /// Marks the meeting failed: a capture that could not start or could
    /// not be saved.
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
/// case-insensitively; blank names dropped), ids derived from the meeting
/// id so a retried `begin` writes the same rows.
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

    /// A phone paired in `store`, and the metadata of a nine-byte upload
    /// from it that started at `now`.
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

    /// `device`'s receipt of the recording `metadata` describes, in
    /// `state`, with its first chunk received at `now`.
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

    #[tokio::test]
    async fn completing_a_meeting_that_is_not_recording_writes_and_enqueues_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path().join("steno.sqlite")).unwrap());
        let mut meeting = steno_core::testing::sample_data::meeting();
        meeting.state = MeetingState::Ready;
        store.save_meeting(&meeting).unwrap();
        let before = store.meeting(meeting.id).unwrap().unwrap();
        let enqueued = Arc::new(AtomicBool::new(false));
        let enqueue: Enqueue = {
            let enqueued = enqueued.clone();
            Arc::new(move |_, _| {
                enqueued.store(true, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            })
        };
        let later = before.updated_at + chrono::Duration::hours(1);
        let intake = LocalRecordingIntake::new(
            store.clone(),
            enqueue,
            Arc::new(move || later),
            FixedOffset::east_opt(0).unwrap(),
        );
        let asset = AudioAsset {
            id: Uuid::new_v4(),
            meeting_id: meeting.id,
            url: file_url(&dir.path().join("recording.wav"), false),
            format: AudioFormat::Wav16kInt16,
            lanes: vec![AudioLane::Mic],
            sidecars_16k: std::collections::BTreeMap::new(),
            retention: AudioRetention::KeepForever,
            expires_at: None,
            mixdown_url: None,
        };
        let error = intake
            .complete(
                meeting.id,
                RecordingResult {
                    asset,
                    duration: 99.0,
                    end_reason: RecordingEndReason::Manual,
                },
                None,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                LocalRecordingIntakeError::NotRecording(id, MeetingStateKind::Ready)
                    if id == meeting.id
            ),
            "{error}"
        );
        assert_eq!(
            store.meeting(meeting.id).unwrap().unwrap(),
            before,
            "the row is untouched and not marked failed"
        );
        assert!(!enqueued.load(Ordering::SeqCst));
    }

    /// A Mac recording `begin` wrote, and what its capture handed back.
    fn a_recording(store: &Arc<Store>, dir: &Path) -> (Meeting, RecordingResult) {
        let at = DateTime::parse_from_rfc3339("2026-09-24T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let intake = LocalRecordingIntake::new(
            store.clone(),
            Arc::new(|_, _| Box::pin(async { Ok(()) })),
            Arc::new(move || at),
            FixedOffset::east_opt(0).unwrap(),
        );
        let meeting = intake
            .begin(MeetingSource::MacInPerson, None, None, &[], at)
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

    /// An enqueue that commits as the pipeline's does (the meeting and the
    /// asset in one transaction, its error wrapped the same way) and
    /// counts its calls; `after_call` runs after each.
    fn committing_enqueue(
        store: Arc<Store>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
        after_call: impl Fn(usize) + Send + Sync + 'static,
    ) -> Enqueue {
        let after_call = Arc::new(after_call);
        Arc::new(move |meeting, asset| {
            let (store, calls, after_call) = (store.clone(), calls.clone(), after_call.clone());
            Box::pin(async move {
                let committed = store
                    .save_meeting_with_asset(&meeting, &asset)
                    .map_err(|error| PipelineFailure::wrapping(&error, PipelineStage::Decode));
                after_call(calls.fetch_add(1, Ordering::SeqCst) + 1);
                committed
            })
        })
    }

    /// A commit that finds another connection holding the database is
    /// tried again and lands; one that keeps finding it leaves the meeting
    /// `recording` (not failed, so the next launch recovers its master)
    /// and returns the busy error.
    #[tokio::test]
    async fn a_busy_commit_is_tried_again_and_one_that_stays_busy_leaves_the_recording() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("steno.sqlite");
        let store = Arc::new(Store::open(&path).unwrap());
        store
            .read(|connection| Ok(connection.busy_timeout(std::time::Duration::from_millis(20))?))
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

        // The lock is released after the last try, so a write after it (a
        // `fail`, as before) would land and show.
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

    /// A commit that fails for another reason (a full disk) is not tried
    /// again and leaves the meeting `recording`, as `begin` wrote it; an
    /// intake set to one try does not retry a busy commit either.
    #[tokio::test]
    async fn a_failed_commit_leaves_the_recording_and_only_a_busy_one_is_tried_again() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path().join("steno.sqlite")).unwrap());
        let (meeting, result) = a_recording(&store, dir.path());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let enqueue: Enqueue = {
            let calls = calls.clone();
            Arc::new(move |_, _| {
                calls.fetch_add(1, Ordering::SeqCst);
                Box::pin(async {
                    Err(PipelineFailure::new(
                        PipelineStage::Decode,
                        "database or disk is full",
                    ))
                })
            })
        };
        let intake = LocalRecordingIntake::new(
            store.clone(),
            enqueue,
            Arc::new(Utc::now),
            FixedOffset::east_opt(0).unwrap(),
        );
        let error = intake.complete(meeting.id, result, None).await.unwrap_err();
        assert!(!error.is_busy(), "{error}");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "not tried again");
        assert_eq!(store.meeting(meeting.id).unwrap().unwrap(), meeting);
        assert!(store.asset(meeting.id).unwrap().is_none());

        let path = dir.path().join("steno.sqlite");
        store
            .read(|connection| Ok(connection.busy_timeout(std::time::Duration::from_millis(20))?))
            .unwrap();
        let (meeting, result) = a_recording(&store, dir.path());
        let hold = WriteLockHold::new(&path);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let intake = LocalRecordingIntake::new(
            store.clone(),
            committing_enqueue(store.clone(), calls.clone(), |_| {}),
            Arc::new(Utc::now),
            FixedOffset::east_opt(0).unwrap(),
        )
        .with_commit_attempts(1);
        let error = intake.complete(meeting.id, result, None).await.unwrap_err();
        drop(hold);
        assert!(error.is_busy(), "{error}");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "one try");
        assert_eq!(store.meeting(meeting.id).unwrap().unwrap(), meeting);
    }

    /// Makes every meeting insert fail, as a full disk or a busy store
    /// would fail the admission's commit; with `failed_receipts_too`, the
    /// save of a `failed` receipt fails as well.
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

    /// A phone recording lands in the audio folder as its meeting's master,
    /// and the upload is deleted. A refused one (a failed admission commit)
    /// leaves a `failed` receipt, the upload for the retry and no copy.
    /// Swift: `admitPlacesTheFileEnqueuesOnceAndIsIdempotent` and
    /// `aFailedAdmissionCommitLeavesAFailedReceiptTheUploadAndNoMeeting`.
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

    /// A failed admission commit leaves no `complete` receipt behind, also
    /// when the save of the `failed` one fails too (a full disk): the
    /// receipt stays as the listener left it, the upload stays for the
    /// retry, and the engine's sweep keeps it. Two separate commits would
    /// leave a `complete` receipt without its meeting, and the phone's
    /// retried `complete` would answer 200 for a meeting that never existed.
    /// The copy stays too: without a durable `failed` receipt over it, a
    /// failed commit whose frames reached the WAL can be replayed after a
    /// crash, and its meeting then needs the copy. Swift:
    /// `aFailedAdmissionWhoseFailedSaveFailsKeepsTheCopyAndNoCompleteReceipt`.
    #[tokio::test]
    async fn a_failed_admission_whose_failed_save_fails_keeps_the_copy_and_no_complete_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_audio_folder(dir.path());
        let now = Utc::now();
        let (enqueue, admitted) = recording_enqueue();
        let intake = phone_intake(&store, enqueue, Arc::new(move || now));
        let (device, metadata) = paired_phone(&store, now);
        store
            .save_handover_receipt(&receipt_of(
                &device,
                &metadata,
                HandoverState::Verifying,
                now,
            ))
            .unwrap();
        let upload = upload_in(dir.path());
        refuse_writes(&store, true);

        intake.admit(&upload, &metadata, &device).await.unwrap_err();

        let receipt = store
            .handover_receipt(metadata.recording_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            receipt.state,
            HandoverState::Verifying,
            "never complete without its meeting"
        );
        assert_eq!(store.all_meetings().unwrap(), []);
        assert!(upload.exists(), "the upload stays for the retry");
        let copies = files_under(&dir.path().join("audio"));
        assert_eq!(copies.len(), 1, "the copy stays: {copies:?}");
        assert_eq!(std::fs::read(&copies[0]).unwrap(), b"aac bytes");
        assert!(admitted.lock().unwrap().is_empty());
    }

    /// A receipt of another upload under the same recording id is never
    /// completed: another phone's (the admitting phone was revoked and the
    /// other one announced the id; the admitting phone then paired again),
    /// or the admitting phone's own of other bytes (it announced another file
    /// under the id), made before the intake read the receipt or between
    /// its read and its commit. The intake refuses, and that receipt stays
    /// as it was: completed, it would answer that upload's `complete` with
    /// this meeting, and the phone would delete a recording never admitted;
    /// no ledger row says those bytes were admitted. Swift:
    /// `aReceiptOfAnotherUploadIsNeverCompleted(afterTheRead:otherBytes:)`.
    #[tokio::test]
    async fn a_receipt_of_another_upload_is_never_completed() {
        for (after_the_read, other_bytes) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let dir = tempfile::tempdir().unwrap();
            let store = store_with_audio_folder(dir.path());
            let now = Utc::now();
            let (device, metadata) = paired_phone(&store, now);
            let other = PairedDevice {
                id: Uuid::new_v4(),
                name: "Other phone".to_owned(),
                ..device.clone()
            };
            // The revoke, the other phone's announce, and this phone pairing
            // again under the same device id; or this phone's announce of
            // another file under the id.
            let owner = if other_bytes { &device } else { &other };
            let theirs = HandoverReceipt {
                sha256: if other_bytes {
                    vec![1; 32]
                } else {
                    metadata.sha256.clone()
                },
                ..receipt_of(owner, &metadata, HandoverState::Receiving, now)
            };
            let takeover = {
                let (store, device, other) = (store.clone(), device.clone(), other.clone());
                let theirs = theirs.clone();
                move || {
                    if !other_bytes {
                        store.delete_paired_device(device.id).unwrap();
                        store.save_paired_device(&other, &[2; 32]).unwrap();
                    }
                    store.save_handover_receipt(&theirs).unwrap();
                    store.save_paired_device(&device, &[1; 32]).unwrap();
                }
            };
            let clock: Now = if after_the_read {
                store
                    .save_handover_receipt(&receipt_of(
                        &device,
                        &metadata,
                        HandoverState::Verifying,
                        now,
                    ))
                    .unwrap();
                // The intake reads the clock once, after its receipt read
                // and before its commit.
                let takeover = Mutex::new(Some(takeover));
                Arc::new(move || {
                    if let Some(takeover) = takeover.lock().unwrap().take() {
                        takeover();
                    }
                    now
                })
            } else {
                takeover();
                Arc::new(move || now)
            };
            let (enqueue, admitted) = recording_enqueue();
            let intake = phone_intake(&store, enqueue, clock);
            let upload = upload_in(dir.path());

            let error = intake.admit(&upload, &metadata, &device).await.unwrap_err();

            assert!(
                error.to_string().contains("belongs to another upload"),
                "{error}"
            );
            let receipt = store
                .handover_receipt(metadata.recording_id)
                .unwrap()
                .unwrap();
            assert_eq!(
                (receipt.device_id, receipt.sha256, receipt.state),
                (theirs.device_id, theirs.sha256, theirs.state),
                "the other upload's receipt is untouched \
                 (after the read: {after_the_read}, other bytes: {other_bytes})"
            );
            assert_eq!(
                store
                    .admitted_meeting(metadata.recording_id, metadata.byte_count, &metadata.sha256)
                    .unwrap(),
                None
            );
            assert_eq!(store.all_meetings().unwrap(), []);
            assert_eq!(
                files_under(&dir.path().join("audio")),
                Vec::<std::path::PathBuf>::new()
            );
            assert!(upload.exists());
            assert!(admitted.lock().unwrap().is_empty());
        }
    }

    /// Once the rows committed, the recording is admitted: an enqueue that
    /// fails then (the pipeline is gone) leaves the meeting `queued` for the
    /// next launch, and the phone is told `complete`. Swift:
    /// `anEnqueueThatFailsAfterTheCommitStillAdmits`.
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

    /// A copy that fails leaves the receipt short of complete, admits
    /// nothing and leaves no meeting folder behind: the phone, told nothing
    /// landed, keeps its recording.
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

    /// A folder already at the new meeting's path (an id that collided)
    /// fails the copy before any write and keeps the recording it holds.
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

    /// A decoder and a dispatcher nothing reaches: the pipeline quits before
    /// the admission, so nothing is processed.
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

    /// One write transaction as the store's commit probe saw it right
    /// before its commit.
    #[derive(Debug, Clone, PartialEq)]
    struct Commit {
        /// `PRAGMA synchronous`: 1 is `NORMAL`, 2 is `FULL`.
        synchronous: i64,
        /// The recording's receipt state, if it has a receipt.
        receipt: Option<String>,
        /// Whether a phone meeting exists.
        meeting: bool,
    }

    /// Every commit on `store` from now on, as [`Commit`]s about
    /// `recording_id`.
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

    /// `PRAGMA synchronous` on the store's connection outside a write.
    fn synchronous(store: &Store) -> i64 {
        store
            .read(
                |connection| Ok(connection.query_row("PRAGMA synchronous", [], |row| row.get(0))?),
            )
            .unwrap()
    }

    /// The production intake commits the `complete` receipt and the meeting in
    /// one transaction under `synchronous = FULL`, and leaves the connection at
    /// `NORMAL`. Every commit is a point a crash could stop at, and that one is
    /// the only one: the enqueue ([`ProcessingPipeline::enqueue_saved`]) writes
    /// nothing. A power loss after the commit cannot be tested; that it ran
    /// under `FULL` can. Swift:
    /// `theProductionIntakeCommitsItsReceiptAndMeetingDurably`.
    #[tokio::test]
    async fn the_production_intake_commits_its_receipt_and_meeting_durably() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_audio_folder(dir.path());
        let now = Utc::now();
        let pipeline = ProcessingPipeline::new(
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
        );
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

    /// A failed admission commit leaves one commit, its `failed` receipt,
    /// under `FULL`, while the copy is still there: a failed commit is not
    /// proof that nothing committed, and the durable `failed` commit is
    /// what writes over a commit a crash could replay. Only then is the
    /// copy removed, and the connection is back at `NORMAL`. Swift:
    /// `aFailedAdmissionCommitSavesFailedDurablyBeforeItRemovesTheCopy`.
    #[tokio::test]
    async fn a_failed_admission_commit_saves_failed_durably_before_it_removes_the_copy() {
        /// A commit's level, the receipt's state and the copies on the disk.
        type Seen = (i64, Option<String>, usize);
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_audio_folder(dir.path());
        let audio = dir.path().join("audio");
        let now = Utc::now();
        let (enqueue, _) = recording_enqueue();
        refuse_writes(&store, false);
        let intake = phone_intake(&store, enqueue, Arc::new(move || now));
        let (device, metadata) = paired_phone(&store, now);
        let upload = upload_in(dir.path());
        let commits: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let seen = commits.clone();
        let (copies, recording_id) = (audio.clone(), metadata.recording_id);
        store.probe_commits(move |connection| {
            let commit = (
                connection
                    .query_row("PRAGMA synchronous", [], |row| row.get(0))
                    .unwrap(),
                connection
                    .query_row(
                        "SELECT (SELECT state FROM handoverReceipt WHERE recordingID = ?1)",
                        [steno_core::store::convert::DbUuid(recording_id)],
                        |row| row.get(0),
                    )
                    .unwrap(),
                files_under(&copies).len(),
            );
            seen.lock().unwrap().push(commit);
        });

        intake.admit(&upload, &metadata, &device).await.unwrap_err();

        assert_eq!(
            *commits.lock().unwrap(),
            [(2, Some("failed".to_owned()), 1)],
            "the failed receipt commits under FULL, with the copy still there"
        );
        assert_eq!(files_under(&audio), Vec::<std::path::PathBuf>::new());
        assert_eq!(synchronous(&store), 1);
    }

    /// The same bytes admitted under another device's receipt (it took the
    /// receipt over while the first admission ran, and wrote it back
    /// unfinished) are the meeting the ledger holds: the receipt is
    /// completed with it, no second meeting is written or enqueued, and
    /// neither the copy nor its folder stays. Swift:
    /// `bytesTheLedgerHoldsAreAdmittedAsTheirMeeting`.
    #[tokio::test]
    async fn bytes_the_ledger_holds_are_admitted_as_their_meeting() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_audio_folder(dir.path());
        let audio = dir.path().join("audio");
        let now = Utc::now();
        let (enqueue, admitted) = recording_enqueue();
        let intake = phone_intake(&store, enqueue, Arc::new(move || now));
        let (older, metadata) = paired_phone(&store, now);
        let first = intake
            .admit(&upload_in(dir.path()), &metadata, &older)
            .await
            .unwrap();
        let newer = PairedDevice {
            id: Uuid::new_v4(),
            ..older.clone()
        };
        store.save_paired_device(&newer, &[2; 32]).unwrap();
        store
            .save_handover_receipt(&receipt_of(
                &newer,
                &metadata,
                HandoverState::Receiving,
                now,
            ))
            .unwrap();
        let upload = upload_in(dir.path());

        let again = intake.admit(&upload, &metadata, &newer).await.unwrap();

        assert_eq!(again, first, "the ledger's meeting");
        assert_eq!(store.all_meetings().unwrap().len(), 1);
        assert_eq!(
            admitted.lock().unwrap().len(),
            1,
            "nothing more is enqueued"
        );
        let receipt = store
            .handover_receipt(metadata.recording_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            (receipt.device_id, receipt.state),
            (newer.id, HandoverState::Complete { meeting_id: first })
        );
        assert_eq!(
            files_under(&audio),
            vec![RecordingLayout::new(&audio, first).master(AudioFormat::M4aAac)],
            "the second copy is gone"
        );
        assert_eq!(
            std::fs::read_dir(&audio).unwrap().count(),
            1,
            "and so is its folder"
        );
        assert!(!upload.exists());
    }

    /// A `complete` receipt whose meeting is gone (the separate receipt and
    /// meeting commits of earlier releases, with a crash or a full disk
    /// between them) is not an idempotent return: the intake admits the
    /// file again into a new meeting, since the phone never got its 200 and
    /// still holds the recording. Swift:
    /// `aCompleteReceiptWhoseMeetingIsGoneIsAdmittedAgain`.
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
