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
/// wiring is [`ProcessingPipeline::enqueue`] for the local intake and
/// [`ProcessingPipeline::enqueue_durably`] for the phone's; tests pass a
/// counting closure.
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
/// audio folder (`RecordingLayout`), writes the `HandoverReceipt` as
/// complete, enqueues a `phone` meeting with a `mixed` asset under the
/// default retention, and only then deletes the upload. Idempotent on
/// `recording_id`.
///
/// The copy and the meeting folder are synced to the disk before the
/// receipt is marked complete ([`crate::files::copy_durably`]), because
/// the phone deletes its own copy once `complete` answers 200. This is
/// deliberately stricter than Swift, whose `RecordingIntake` used
/// `copyItem` and synced nothing, so a power loss after the answer lost
/// the recording on both devices. For the same reason the `complete`
/// receipt and the meeting commit durably ([`Store::write_durably`]),
/// as in Swift: the receipt here, the meeting in the enqueue, which is
/// [`ProcessingPipeline::enqueue_durably`] in [`RecordingIntake::over`]
/// and must be in any other production `enqueue`. The `failed` receipt of
/// a refused admission commits as usual: the phone keeps its copy then.
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

    /// The production wiring over `pipeline`, whose meeting commits
    /// durably ([`ProcessingPipeline::enqueue_durably`]).
    #[must_use]
    pub fn over(store: Arc<Store>, pipeline: ProcessingPipeline, zone: FixedOffset) -> Self {
        let now = pipeline.dependencies().now.clone();
        Self::new(
            store,
            enqueue_through(pipeline, ProcessingPipeline::enqueue_durably),
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

        let admitted: BoundaryResult<()> = async {
            self.store.save_handover_receipt_durably(&receipt)?;
            (self.enqueue)(meeting, asset).await?;
            Ok(())
        }
        .await;
        if let Err(error) = admitted {
            let _ = std::fs::remove_file(&destination);
            receipt.state = HandoverState::Failed(format!("admit: {error}"));
            let _ = self.store.save_handover_receipt(&receipt);
            return Err(error);
        }
        let _ = std::fs::remove_file(file);
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

    #[tokio::test]
    async fn a_phone_recording_lands_in_the_audio_folder_and_a_refused_one_leaves_no_copy() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_audio_folder(dir.path());
        let audio = dir.path().join("audio");
        let now = DateTime::parse_from_rfc3339("2026-09-24T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let admitted: Arc<Mutex<Vec<(Meeting, AudioAsset)>>> = Arc::default();
        let refusing = Arc::new(AtomicBool::new(false));
        let enqueue: Enqueue = {
            let (admitted, refusing) = (admitted.clone(), refusing.clone());
            Arc::new(move |meeting, asset| {
                let admitted = admitted.clone();
                let refuse = refusing.load(Ordering::SeqCst);
                Box::pin(async move {
                    if refuse {
                        return Err(PipelineFailure::new(PipelineStage::Decode, "no runtime"));
                    }
                    admitted.lock().unwrap().push((meeting, asset));
                    Ok(())
                })
            })
        };
        let intake = RecordingIntake::new(
            store.clone(),
            enqueue,
            Arc::new(move || now),
            FixedOffset::east_opt(0).unwrap(),
        );
        let (device, metadata) = paired_phone(&store, now);
        let upload = dir.path().join("upload.m4a");
        std::fs::write(&upload, b"aac bytes").unwrap();

        let meeting_id = intake.admit(&upload, &metadata, &device).await.unwrap();
        let (meeting, asset) = admitted.lock().unwrap()[0].clone();
        assert_eq!(meeting.id, meeting_id);
        assert_eq!(meeting.title, "Phone recording 2026-09-24 09:00");
        let master = RecordingLayout::new(&audio, meeting_id).master(AudioFormat::M4aAac);
        assert!(master.starts_with(&audio));
        assert_eq!(asset.url, file_url(&master, false));
        assert_eq!(asset.lanes, vec![AudioLane::Mixed]);
        assert_eq!(std::fs::read(&master).unwrap(), b"aac bytes");
        assert!(!upload.exists(), "the upload is deleted once enqueued");
        assert!(matches!(
            store.handover_receipt(metadata.recording_id).unwrap().unwrap().state,
            HandoverState::Complete { meeting_id: id } if id == meeting_id
        ));

        refusing.store(true, Ordering::SeqCst);
        std::fs::write(&upload, b"aac bytes").unwrap();
        let second = RecordingMetadata {
            recording_id: Uuid::new_v4(),
            ..metadata.clone()
        };
        let error = intake.admit(&upload, &second, &device).await.unwrap_err();
        assert!(error.to_string().contains("no runtime"), "{error}");
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
        assert_eq!(admitted.lock().unwrap().len(), 1);
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
        let intake = RecordingIntake::new(
            store.clone(),
            enqueue,
            Arc::new(move || now),
            FixedOffset::east_opt(0).unwrap(),
        );
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
    /// meeting under `synchronous = FULL`, each the first commit that holds
    /// it, and leaves the connection at `NORMAL`. A power loss after the
    /// commits cannot be tested; that they ran under `FULL` can.
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
        let upload = dir.path().join("upload.m4a");
        std::fs::write(&upload, b"aac bytes").unwrap();
        assert_eq!(synchronous(&store), 1);
        let commits = commits_of(&store, metadata.recording_id);

        let meeting_id = intake.admit(&upload, &metadata, &device).await.unwrap();

        let commits = commits.lock().unwrap().clone();
        let receipt = commits
            .iter()
            .find(|commit| commit.receipt.as_deref() == Some("complete"))
            .expect("a commit wrote the complete receipt");
        assert_eq!(receipt.synchronous, 2, "the receipt commits under FULL");
        let meeting = commits
            .iter()
            .find(|commit| commit.meeting)
            .expect("a commit wrote the meeting");
        assert_eq!(meeting.synchronous, 2, "the meeting commits under FULL");
        assert_eq!(synchronous(&store), 1, "the connection is back at NORMAL");
        assert!(store.meeting(meeting_id).unwrap().is_some());
    }

    /// A refused admission's `failed` receipt commits as usual, after the
    /// `complete` one committed under `FULL`, and the connection is back
    /// at `NORMAL`.
    #[tokio::test]
    async fn a_refused_admission_leaves_the_connection_at_normal() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_with_audio_folder(dir.path());
        let now = Utc::now();
        let enqueue: Enqueue = Arc::new(|_, _| {
            Box::pin(async { Err(PipelineFailure::new(PipelineStage::Decode, "no runtime")) })
        });
        let intake = RecordingIntake::new(
            store.clone(),
            enqueue,
            Arc::new(move || now),
            FixedOffset::east_opt(0).unwrap(),
        );
        let (device, metadata) = paired_phone(&store, now);
        let upload = dir.path().join("upload.m4a");
        std::fs::write(&upload, b"aac bytes").unwrap();
        let commits = commits_of(&store, metadata.recording_id);

        intake.admit(&upload, &metadata, &device).await.unwrap_err();

        let levels: Vec<(i64, Option<String>)> = commits
            .lock()
            .unwrap()
            .iter()
            .map(|commit| (commit.synchronous, commit.receipt.clone()))
            .collect();
        assert_eq!(
            levels,
            [
                (2, Some("complete".to_owned())),
                (1, Some("failed".to_owned())),
            ]
        );
        assert_eq!(synchronous(&store), 1);
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
