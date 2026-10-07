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
    AudioAsset, AudioLane, AudioRetention, HandoverIntake, HandoverReceipt, HandoverState, Meeting,
    MeetingSource, MeetingState, MeetingStateKind, PairedDevice, Participant, ParticipantRole,
    RecordingEndReason, RecordingLayout, RecordingMetadata, Store, StoreError, TitleOrigin,
    async_trait, derived_uuid, paths::file_url, paths::file_url_path, protocols::BoundaryResult,
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
/// ([`crate::files::create_dir_all_durably`],
/// [`crate::files::copy_durably`]); Swift syncs its `copyItem` copy and
/// the folders the same way. The receipt, the meeting and its asset
/// commit in one durable transaction ([`Store::save_admission_durably`]),
/// so no crash, full disk or busy store leaves a `complete` receipt
/// without its meeting. When that commit fails, the receipt is saved
/// `failed` durably ([`Store::save_handover_receipt_durably`]), and the
/// copy is removed only once that save succeeds: a failed commit can still
/// be replayed after a crash, and its meeting then needs the copy. The
/// phone keeps its own copy either way.
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
        // Another phone's receipt under this id (this one was revoked, and
        // that one announced the id) is never completed or answered from:
        // that phone would take this meeting for its own and delete its copy.
        if existing
            .as_ref()
            .is_some_and(|receipt| receipt.device_id != device.id)
        {
            return Err(StoreError::ReceiptOfAnotherDevice(metadata.recording_id).into());
        }
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
        crate::files::create_dir_all_durably(&layout.directory)?;
        let destination = layout.master(metadata.format);
        crate::files::copy_durably(file, &destination)?;

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

        if let Err(error) = self
            .store
            .save_admission_durably(&receipt, &meeting, &asset)
        {
            if matches!(error, StoreError::ReceiptOfAnotherDevice(_)) {
                // The refusal wrote nothing, and another phone's receipt is
                // left as it is.
                let _ = std::fs::remove_file(&destination);
            } else {
                // A failed commit is not proof that nothing committed: a
                // WAL sync that fails leaves the commit's frames in the WAL,
                // and recovery after a crash replays them. The durable
                // `failed` save writes over them, so the copy goes only once
                // that save is on the disk; otherwise it stays, an orphan at
                // worst, and a replayed admission still finds its master.
                receipt.state = HandoverState::Failed(format!("admit: {error}"));
                if self.store.save_handover_receipt_durably(&receipt).is_ok() {
                    let _ = std::fs::remove_file(&destination);
                }
            }
            return Err(error.into());
        }
        let _ = std::fs::remove_file(file);
        // Admitted: a pipeline that cannot take the meeting now leaves it
        // `queued`, and the next launch resumes it.
        if let Err(failure) = (self.enqueue)(meeting, asset).await {
            tracing::warn!(%meeting_id, stage = failure.stage.as_str(), "admitted meeting not enqueued");
        }
        Ok(meeting_id)
    }
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

/// The Mac recording transaction. `begin` writes the `recording` row the
/// list shows while the capture session runs; `complete` turns the
/// finished capture into a `queued` meeting with its asset and hands both
/// to the pipeline; `fail` records why a recording never made it.
pub struct LocalRecordingIntake {
    store: Arc<Store>,
    enqueue: Enqueue,
    now: Now,
    zone: FixedOffset,
}

impl LocalRecordingIntake {
    #[must_use]
    pub fn new(store: Arc<Store>, enqueue: Enqueue, now: Now, zone: FixedOffset) -> Self {
        LocalRecordingIntake {
            store,
            enqueue,
            now,
            zone,
        }
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

    /// Writes the duration and the end reason, sets the asset's retention
    /// (`retention`, else the settings' default as it is now) with
    /// `expires_at` cleared, and enqueues the meeting. A meeting that is not
    /// `recording` is left alone, its row untouched; any other failure
    /// marks it failed.
    pub async fn complete(
        &self,
        meeting_id: Uuid,
        result: RecordingResult,
        retention: Option<AudioRetention>,
    ) -> Result<Meeting, LocalRecordingIntakeError> {
        match self.complete_inner(meeting_id, result, retention).await {
            Ok(meeting) => Ok(meeting),
            Err(error @ LocalRecordingIntakeError::NotRecording(..)) => Err(error),
            Err(error) => {
                let _ = self.fail(
                    meeting_id,
                    &format!("Recording could not be saved: {error}"),
                );
                Err(error)
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
        let mut meeting = self
            .store
            .update_meeting(meeting_id, timestamp, |meeting| {
                meeting.duration = result.duration;
                meeting.end_reason = Some(result.end_reason.clone());
                Ok(())
            })?;
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

    use steno_core::{AudioFormat, PipelineStage};

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
    /// crash, and its meeting then needs the copy.
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

    /// A receipt of another phone under the same recording id is never
    /// completed. The admitting phone was revoked and the other one
    /// announced the id, before the intake read the receipt or between its
    /// read and its commit; either way the admitting phone then paired
    /// again. The intake refuses, and the other phone's receipt stays as it was:
    /// completed, it would answer that phone's `complete` with this meeting,
    /// and that phone would delete a recording never admitted.
    #[tokio::test]
    async fn a_receipt_of_another_phone_is_never_completed() {
        for after_the_read in [false, true] {
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
            // again under the same device id.
            let takeover = {
                let (store, device, other) = (store.clone(), device.clone(), other.clone());
                let theirs = receipt_of(&other, &metadata, HandoverState::Receiving, now);
                move || {
                    store.delete_paired_device(device.id).unwrap();
                    store.save_paired_device(&other, &[2; 32]).unwrap();
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
                error.to_string().contains("belongs to another device"),
                "{error}"
            );
            let receipt = store
                .handover_receipt(metadata.recording_id)
                .unwrap()
                .unwrap();
            assert_eq!(
                (receipt.device_id, receipt.state),
                (other.id, HandoverState::Receiving),
                "the other phone's receipt is untouched (after the read: {after_the_read})"
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
    /// fails then (the pipeline is gone) leaves the meeting `queued` for
    /// the next launch, and the phone is told `complete`.
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

    /// A copy that fails leaves the receipt short of complete and admits
    /// nothing: the phone, told nothing landed, keeps its recording.
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

    /// The production intake commits the `complete` receipt and the
    /// meeting in one transaction under `synchronous = FULL`, and leaves
    /// the connection at `NORMAL`. Every commit is a point a crash could
    /// stop at, and that one is the only one: the enqueue
    /// ([`ProcessingPipeline::enqueue_saved`]) writes nothing. A power
    /// loss after the commit cannot be tested; that it ran under `FULL`
    /// can.
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

    /// A refused admission commits nothing but its `failed` receipt, and
    /// that one under `FULL`, while the copy is still there: a failed
    /// commit is not proof that nothing committed, and the durable `failed`
    /// commit is what writes over a commit a crash could replay. Only then
    /// is the copy removed, and the connection is back at `NORMAL`.
    #[tokio::test]
    async fn a_refused_admission_saves_failed_durably_before_it_removes_the_copy() {
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
