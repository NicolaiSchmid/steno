//! `steno process <wav>`: copies the 16 kHz mono WAV (and the system lane
//! for a call) into `<audio folder>/<meetingID>/`, enqueues the meeting and
//! waits for the pipeline. Prints one line per progress event to standard
//! error, `stage percent remaining`, and the meeting id alone to standard
//! output; a note on stderr says when the summary was skipped for lack of
//! an LLM endpoint. Swift: `Sources/steno/Commands/Process.swift`.
//!
//! `steno process --meeting <id>` processes a stored ready or failed
//! meeting again from its recording, through the pipeline's `reprocess`,
//! and reports as above. Rust only: Swift's CLI had no such flag.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{Duration, Utc};
use clap::{Args, ValueEnum};
use steno_core::{
    AudioAsset, AudioFormat, AudioLane, Meeting, MeetingEvent, MeetingSource, MeetingState,
    PipelineStage, ProcessingProgress, RecordingLayout, Settings, Store, SummaryTemplate,
    TitleOrigin,
    paths::{file_url, file_url_path},
};
use steno_pipeline::{MeetingEventBus, ProcessingPipeline, ReprocessError};
use uuid::Uuid;

use crate::wiring::{DatabaseOptions, Failure, Outcome, SpeechOptions, parse_uuid};

/// `--source mac-call|mac-in-person|phone`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Source {
    MacCall,
    MacInPerson,
    Phone,
}

impl From<Source> for MeetingSource {
    fn from(source: Source) -> Self {
        match source {
            Source::MacCall => MeetingSource::MacCall,
            Source::MacInPerson => MeetingSource::MacInPerson,
            Source::Phone => MeetingSource::Phone,
        }
    }
}

#[derive(Debug, Args)]
pub struct Process {
    /// 16 kHz mono WAV: the mic lane of a call, or the room recording.
    #[arg(required_unless_present = "meeting")]
    pub input: Option<PathBuf>,
    /// Process a stored ready or failed meeting again from its recording,
    /// instead of a new one from <INPUT>.
    #[arg(
        long,
        value_name = "ID",
        value_parser = parse_uuid,
        conflicts_with_all = ["input", "system_lane", "source", "title", "template", "audio_folder"]
    )]
    pub meeting: Option<Uuid>,
    /// The system lane WAV of a call.
    #[arg(long = "system-lane", value_name = "WAV")]
    pub system_lane: Option<PathBuf>,
    /// mac-call, mac-in-person or phone.
    #[arg(long, value_enum, default_value_t = Source::MacInPerson)]
    pub source: Source,
    /// Meeting title; defaults to the input file name.
    #[arg(long)]
    pub title: Option<String>,
    /// Summary template id; defaults to the settings' default template.
    #[arg(long)]
    pub template: Option<String>,
    /// Where this meeting's folder is created; defaults to the settings' audio folder.
    #[arg(long = "audio-folder", value_name = "DIR")]
    pub audio_folder: Option<PathBuf>,
    #[command(flatten)]
    pub database: DatabaseOptions,
    #[command(flatten)]
    pub speech: SpeechOptions,
}

impl Process {
    fn validate(&self, input: &Path) -> Result<(), Failure> {
        self.speech.validate()?;
        match self.source {
            Source::MacCall if self.system_lane.is_none() => {
                return Err(Failure::usage(
                    "--source mac-call needs --system-lane <wav>.",
                ));
            }
            Source::MacInPerson | Source::Phone if self.system_lane.is_some() => {
                return Err(Failure::usage(
                    "--system-lane only applies to --source mac-call.",
                ));
            }
            _ => {}
        }
        if let Some(template) = &self.template
            && SummaryTemplate::bundled_with_id(template).is_none()
        {
            return Err(Failure::usage(format!(
                "Unknown template {template}. Bundled: {}.",
                SummaryTemplate::BUNDLED_IDS.join(", ")
            )));
        }
        if !input.exists() {
            return Err(Failure::usage(format!("No such file: {}", input.display())));
        }
        if let Some(lane) = &self.system_lane
            && !lane.exists()
        {
            return Err(Failure::usage(format!("No such file: {}", lane.display())));
        }
        Ok(())
    }

    pub async fn run(self) -> Outcome {
        match (self.meeting, self.input.clone()) {
            (Some(meeting_id), _) => self.run_again(meeting_id).await,
            (None, Some(input)) => self.run_new(&input).await,
            // clap requires one of the two.
            (None, None) => Err(Failure::usage("Name a WAV to process, or --meeting <ID>.")),
        }
    }

    /// A new meeting from `input`: copy, then enqueue, print and wait.
    async fn run_new(self, input: &Path) -> Outcome {
        self.validate(input)?;
        let store = self.database.open()?;
        let settings = store.settings().map_err(Failure::runtime)?;
        // `--audio-folder` is a plain path for this run; the stored setting
        // is the app's and never changes here.
        let root = match &self.audio_folder {
            Some(folder) => crate::wiring::standardized(folder),
            None => file_url_path(&settings.audio_folder)
                .ok_or_else(|| Failure::runtime("the audio folder setting is not a file URL"))?,
        };
        let meeting_id = Uuid::new_v4();
        let layout = RecordingLayout::new(&root, meeting_id);
        layout.create_directories(false).map_err(Failure::runtime)?;
        // The header alone: a two-hour lane is not read whole for its length.
        let duration = steno_audio::WavFile::read_duration(input)
            .map_err(|e| Failure::runtime(format!("{}: {e}", input.display())))?;

        let asset = if let (Source::MacCall, Some(system_lane)) = (self.source, &self.system_lane) {
            let mic = layout.sidecar(AudioLane::Mic);
            let system = layout.sidecar(AudioLane::System);
            std::fs::copy(input, &mic).map_err(Failure::runtime)?;
            std::fs::copy(system_lane, &system).map_err(Failure::runtime)?;
            AudioAsset {
                id: Uuid::new_v4(),
                meeting_id,
                url: file_url(&mic, false),
                format: AudioFormat::Wav16kInt16,
                lanes: vec![AudioLane::Mic, AudioLane::System],
                sidecars_16k: BTreeMap::from([
                    (AudioLane::Mic, file_url(&mic, false)),
                    (AudioLane::System, file_url(&system, false)),
                ]),
                mixdown_url: None,
                retention: settings.default_retention,
                expires_at: None,
            }
        } else {
            let recording = layout.master(AudioFormat::Wav16kInt16);
            std::fs::copy(input, &recording).map_err(Failure::runtime)?;
            AudioAsset {
                id: Uuid::new_v4(),
                meeting_id,
                url: file_url(&recording, false),
                format: AudioFormat::Wav16kInt16,
                lanes: vec![AudioLane::Mixed],
                sidecars_16k: BTreeMap::new(),
                mixdown_url: None,
                retention: settings.default_retention,
                expires_at: None,
            }
        };

        let now = Utc::now();
        let (title, title_origin) = title_and_origin(self.title.as_deref(), input);
        // Whole milliseconds of a recording's length.
        #[allow(clippy::cast_possible_truncation)]
        let length = Duration::milliseconds((duration * 1000.0) as i64);
        let meeting = Meeting {
            id: meeting_id,
            title,
            started_at: now - length,
            duration,
            language: None,
            source: self.source.into(),
            calendar_event_id: None,
            tags: Vec::new(),
            state: MeetingState::Queued,
            end_reason: None,
            title_origin,
            template_id: self
                .template
                .clone()
                .unwrap_or_else(|| settings.default_template_id.clone()),
            summary: None,
            scratchpad: String::new(),
            llm_usage: None,
            created_at: now,
            updated_at: now,
        };

        self.run_and_report(&store, &settings, meeting_id, |pipeline| {
            pipeline.enqueue(&meeting, &asset).map_err(Failure::runtime)
        })
        .await
    }

    /// `--meeting`: the stored meeting processed again from its recording.
    async fn run_again(self, meeting_id: Uuid) -> Outcome {
        self.speech.validate()?;
        let store = self.database.open()?;
        let settings = store.settings().map_err(Failure::runtime)?;
        self.run_and_report(&store, &settings, meeting_id, |pipeline| {
            pipeline.reprocess(meeting_id).map_err(reprocess_failure)
        })
        .await
    }

    /// Builds the pipeline from the settings and the speech flags, starts
    /// `meeting_id`'s run with `start`, prints its progress, waits for it
    /// and reports how it ended.
    async fn run_and_report(
        &self,
        store: &Arc<Store>,
        settings: &Settings,
        meeting_id: Uuid,
        start: impl FnOnce(&ProcessingPipeline) -> Result<(), Failure>,
    ) -> Outcome {
        let llm = crate::wiring::llm_passes(settings).await?;
        let skipped = llm.is_none();
        let events = MeetingEventBus::new();
        let mut receiver = events.subscribe();
        let models = settings.models_directory.as_deref().and_then(file_url_path);
        let pipeline = ProcessingPipeline::new(crate::wiring::dependencies(
            store.clone(),
            settings,
            self.speech.engine.as_deref(),
            models.as_deref(),
            None,
            llm,
            events.clone(),
        )?);
        let printer = tokio::spawn(async move {
            while let Ok(event) = receiver.recv().await {
                if let MeetingEvent::Progress {
                    meeting_id: id,
                    progress,
                } = event
                    && id == meeting_id
                {
                    eprintln!("{}", progress_line(&progress));
                }
            }
        });
        start(&pipeline)?;
        pipeline.wait_until_idle().await;
        drop(events);
        drop(pipeline);
        let _ = printer.await;

        let result = store
            .meeting(meeting_id)
            .map_err(Failure::runtime)?
            .ok_or_else(|| {
                Failure::runtime(format!("meeting {meeting_id} vanished during processing"))
            })?;
        if let MeetingState::Failed { reason } = &result.state {
            return Err(Failure::runtime(format!("processing failed: {reason}")));
        }
        if skipped {
            eprintln!("summary skipped: no LLM endpoint configured");
        }
        println!("{}", steno_core::json::uuid_string(meeting_id));
        Ok(())
    }
}

/// What `--meeting` prints when the pipeline refuses the meeting. An id
/// no meeting has is a usage error, like an unknown template.
fn reprocess_failure(error: ReprocessError) -> Failure {
    match error {
        ReprocessError::MeetingNotFound(id) => Failure::usage(format!(
            "No meeting has the id {}.",
            steno_core::json::uuid_string(id)
        )),
        ReprocessError::Unfinished { meeting_id, state } => Failure::runtime(format!(
            "Meeting {} is {}; only a ready or failed meeting can be processed again.",
            steno_core::json::uuid_string(meeting_id),
            state.as_str()
        )),
        ReprocessError::NoAsset(id) => Failure::runtime(format!(
            "Meeting {} has no recording on record, so it cannot be processed again.",
            steno_core::json::uuid_string(id)
        )),
        ReprocessError::AudioGone(id) => Failure::runtime(format!(
            "The recording of meeting {} is no longer on disk, so it cannot be processed again.",
            steno_core::json::uuid_string(id)
        )),
        ReprocessError::Busy(id) => Failure::runtime(format!(
            "Meeting {} is already being processed.",
            steno_core::json::uuid_string(id)
        )),
        ReprocessError::Quitting => {
            Failure::runtime("The pipeline is shutting down; nothing was started.")
        }
        ReprocessError::Pipeline(failure) => {
            Failure::runtime(format!("Processing could not start: {failure}"))
        }
    }
}

/// `stage percent remaining`, the stage padded to the longest name so the
/// percents line up, and the lane when the stage runs over more than one:
/// `transcribe     3% 1m 20s, lane 2 of 2`.
#[must_use]
pub fn progress_line(progress: &ProcessingProgress) -> String {
    let width = PipelineStage::ALL
        .iter()
        .map(|stage| stage.as_str().len())
        .max()
        .unwrap_or(0);
    // A percentage, 0 to 100.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let percent = (progress.fraction * 100.0).floor() as u32;
    let lane = if progress.lane_count > 1 {
        format!(", lane {} of {}", progress.lane + 1, progress.lane_count)
    } else {
        String::new()
    };
    format!(
        "{:<width$} {percent:>3}% {}{lane}",
        progress.stage.as_str(),
        remaining_text(progress.estimated_remaining_seconds)
    )
}

/// `1m 20s` or `4s`, seconds rounded up so a run never reads as done early.
#[must_use]
pub fn remaining_text(remaining: f64) -> String {
    // Whole seconds of a bounded estimate.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let seconds = remaining.max(0.0).ceil() as u64;
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {}s", seconds / 60, seconds % 60)
    }
}

/// The meeting's title for `--title`, else the file name of `input`. A
/// given title is the user's: the app shows it, as the export does, and
/// the summary keeps it. Swift's CLI stores it as the default title (see
/// the parity list). It is stored trimmed, and an empty or blank one is no
/// title, as in the intake.
fn title_and_origin(given: Option<&str>, input: &Path) -> (String, TitleOrigin) {
    match given.map(str::trim) {
        Some(title) if !title.is_empty() => (title.to_owned(), TitleOrigin::User),
        _ => (
            input
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default(),
            TitleOrigin::Default,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_lines_pad_and_name_the_lane() {
        let line = progress_line(&ProcessingProgress {
            stage: PipelineStage::Transcribe,
            fraction: 0.034,
            next_fraction: 0.2,
            estimated_remaining_seconds: 80.0,
            is_estimate_seeded: true,
            lane: 1,
            lane_count: 2,
        });
        assert_eq!(line, "transcribe      3% 1m 20s, lane 2 of 2");
        assert_eq!(remaining_text(3.2), "4s");
    }

    #[test]
    fn a_given_title_is_the_users_unless_it_is_blank() {
        let input = Path::new("/recordings/standup.wav");
        for given in ["Sweep", " Sweep "] {
            assert_eq!(
                title_and_origin(Some(given), input),
                ("Sweep".to_owned(), TitleOrigin::User),
                "{given:?}"
            );
        }
        for blank in [None, Some(""), Some("  \t")] {
            assert_eq!(
                title_and_origin(blank, input),
                ("standup".to_owned(), TitleOrigin::Default),
                "{blank:?}"
            );
        }
    }
}
