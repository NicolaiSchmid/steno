//! `steno dev`: developer tools. Swift: `Sources/steno/Commands/Dev*.swift`.
//! `capture-spike` and `audio-devices` need the platform's live capture and
//! device enumeration, which exist on the Mac; elsewhere they parse their
//! flags and report that the backend is not available yet.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use clap::{Args, Subcommand, ValueEnum};
use steno_audio::testing::AudioFixtures;
use steno_audio::{EchoMetrics, PassthroughEchoCanceller, SpeexEchoCanceller, WavFile};
use steno_core::{
    AudioBuffer16k, AudioLane, Diarizer, EchoCanceller, MeetingExport, MeetingSummarizer,
    SpeechEngine, SummaryTemplate, TranscriptCleaner, paths::path_from_file_url,
};
use steno_diarize::{DiarizerConfig, ModelDiarizer};
use steno_host::speech::ModelAsset;
use steno_llm::{LlmClient, LlmEndpoint, LlmMeetingSummarizer, LlmTranscriptCleaner, RetryPolicy};
use steno_services::speech::ModelStoreSpeechModels;
use steno_speech_coreml::wer;

use crate::wiring::{DatabaseOptions, Failure, Outcome, sha256_hex};

#[derive(Debug, Args)]
pub struct Dev {
    #[command(subcommand)]
    pub command: DevCommand,
}

#[derive(Debug, Subcommand)]
pub enum DevCommand {
    /// Database maintenance.
    Db(Db),
    /// Generated test fixtures.
    Fixtures(Fixtures),
    /// List audio devices and the processes using them.
    AudioDevices(AudioDevices),
    /// Measure echo cancellation on recorded or synthetic lanes.
    AecBench(AecBench),
    /// Record the live lanes and report levels, layout and onset alignment.
    CaptureSpike(CaptureSpike),
    /// List, download or remove speech and diarization models.
    Models(Models),
    /// Compare speech engines over a folder of recordings.
    Bakeoff(Bakeoff),
    /// Diarize recordings at several clustering thresholds and report the clusters.
    DiarizeSweep(DiarizeSweep),
    /// Probe the LLM endpoint, run the cleanup or the summary pass over a meeting.json.
    Llm(Llm),
    /// Developer tools for the phone handover.
    Handover(Handover),
}

impl Dev {
    pub async fn run(self) -> Outcome {
        match self.command {
            DevCommand::Db(command) => command.run(),
            DevCommand::Fixtures(command) => command.run(),
            DevCommand::AudioDevices(command) => command.run(),
            DevCommand::AecBench(command) => command.run(),
            DevCommand::CaptureSpike(command) => command.run().await,
            DevCommand::Models(command) => command.run(),
            DevCommand::Bakeoff(command) => command.run().await,
            DevCommand::DiarizeSweep(command) => command.run().await,
            DevCommand::Llm(command) => command.run().await,
            DevCommand::Handover(command) => command.run().await,
        }
    }
}

// db

#[derive(Debug, Args)]
pub struct Db {
    #[command(subcommand)]
    pub command: DbCommand,
}

#[derive(Debug, Subcommand)]
pub enum DbCommand {
    /// Create or migrate the database.
    Migrate(DatabaseOptions),
    /// Rebuild the full-text indexes.
    Reindex(DatabaseOptions),
}

impl Db {
    fn run(self) -> Outcome {
        match self.command {
            DbCommand::Migrate(database) => {
                let path = database.path()?;
                let store = database.open()?;
                let applied = store.applied_migrations().map_err(Failure::runtime)?;
                println!("{}: {}", path.display(), applied.join(", "));
                Ok(())
            }
            DbCommand::Reindex(database) => {
                let path = database.path()?;
                database
                    .open()?
                    .rebuild_search_index()
                    .map_err(Failure::runtime)?;
                println!("reindexed {}", path.display());
                Ok(())
            }
        }
    }
}

// fixtures

#[derive(Debug, Args)]
pub struct Fixtures {
    #[command(subcommand)]
    pub command: FixturesCommand,
}

#[derive(Debug, Subcommand)]
pub enum FixturesCommand {
    /// Write every generated audio fixture and MANIFEST.sha256 into a directory.
    Generate {
        /// Target directory, for example Tests/Fixtures.
        #[arg(long)]
        out: PathBuf,
    },
}

impl Fixtures {
    fn run(self) -> Outcome {
        let FixturesCommand::Generate { out } = self.command;
        let outputs =
            steno_pipeline::fixtures::generate(&out, &sha256_hex).map_err(Failure::runtime)?;
        std::fs::write(
            out.join("MANIFEST.sha256"),
            steno_pipeline::fixtures::manifest(&outputs),
        )
        .map_err(Failure::runtime)?;
        for output in outputs {
            println!("{}  {}", output.sha256, output.relative_path);
        }
        Ok(())
    }
}

// audio-devices

#[derive(Debug, Args)]
pub struct AudioDevices {
    /// Only processes with a running input stream.
    #[arg(long = "running-only")]
    pub running_only: bool,
}

impl AudioDevices {
    fn run(self) -> Outcome {
        #[cfg(target_os = "macos")]
        {
            let source = steno_audio::detection::LiveProcessAudioActivity::new();
            let activity = steno_audio::ProcessAudioActivitySource::snapshot(&source)
                .map_err(Failure::runtime)?;
            for entry in activity {
                let running = entry.is_running_input;
                if self.running_only && !running {
                    continue;
                }
                println!(
                    "pid {} {} {}",
                    entry.pid,
                    entry.bundle_id.as_deref().unwrap_or("-"),
                    if running { "[input running]" } else { "" }
                );
            }
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = self.running_only;
            Err(Failure::runtime(
                "audio-devices: device enumeration is only available on the Mac.",
            ))
        }
    }
}

// aec-bench

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AecEngine {
    Speex,
    Passthrough,
}

#[derive(Debug, Args)]
pub struct AecBench {
    /// The microphone lane before cancellation (a WAV).
    #[arg(long)]
    pub mic: Option<PathBuf>,
    /// The far-end lane (a WAV).
    #[arg(long)]
    pub far: Option<PathBuf>,
    /// speex or passthrough.
    #[arg(long, value_enum, default_value_t = AecEngine::Speex)]
    pub engine: AecEngine,
    /// Echo tail in milliseconds.
    #[arg(long = "tail-milliseconds", default_value_t = 200)]
    pub tail_milliseconds: usize,
    /// Write the processed mic lane as 48 kHz WAV here.
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Use the synthetic six-second echo fixtures instead of --mic and --far.
    #[arg(long)]
    pub synthetic: bool,
}

impl AecBench {
    fn run(self) -> Outcome {
        let (near, far) = if self.synthetic {
            let far = AudioFixtures::speech_like_far(6.0);
            let mic = AudioFixtures::echo_mic(&far, &AudioFixtures::room_impulse_response());
            (mic, far)
        } else {
            let (Some(mic), Some(far)) = (&self.mic, &self.far) else {
                return Err(Failure::usage(
                    "aec-bench needs --mic and --far, or --synthetic for the built-in fixtures.",
                ));
            };
            (read_48k(mic)?, read_48k(far)?)
        };
        let frame = steno_audio::FRAME_SIZE;
        let rate = steno_audio::SAMPLE_RATE;
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let tail = (rate * self.tail_milliseconds as f64 / 1000.0) as usize;
        let mut canceller: Box<dyn EchoCanceller> = match self.engine {
            AecEngine::Speex => Box::new(
                SpeexEchoCanceller::with_tail(rate, frame, tail.max(frame), -40, -15)
                    .map_err(Failure::runtime)?,
            ),
            AecEngine::Passthrough => Box::new(PassthroughEchoCanceller::new(rate, frame)),
        };
        let processed = EchoMetrics::run(canceller.as_mut(), &near, &far, frame);
        println!(
            "engine: {}, tail {} ms",
            match self.engine {
                AecEngine::Speex => "speex",
                AecEngine::Passthrough => "passthrough",
            },
            self.tail_milliseconds
        );
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let second = rate as usize;
        for seconds in 1..=(processed.len() / second) {
            let range = (seconds - 1) * second..seconds * second;
            let erle = EchoMetrics::erle(&near, &processed, range);
            println!("after {seconds} s {erle:.1} dB");
        }
        if let Some(out) = &self.out {
            let samples: Vec<i16> = processed
                .iter()
                .map(|s| {
                    #[allow(clippy::cast_possible_truncation)]
                    let q = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
                    q
                })
                .collect();
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let data = steno_pipeline::fixtures::wav_data(&samples, rate as u32, 1);
            std::fs::write(out, data).map_err(Failure::runtime)?;
        }
        Ok(())
    }
}

fn read_48k(path: &Path) -> Result<Vec<f32>, Failure> {
    let file =
        WavFile::read(path).map_err(|e| Failure::runtime(format!("{}: {e}", path.display())))?;
    file.channels
        .into_iter()
        .next()
        .ok_or_else(|| Failure::runtime(format!("{}: no channels", path.display())))
}

// capture-spike

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Lanes {
    System,
    Call,
    InPerson,
}

#[derive(Debug, Args)]
pub struct CaptureSpike {
    /// How long to record.
    #[arg(long, default_value_t = 10.0)]
    pub seconds: f64,
    /// The audio folder; the meeting folder is created inside it.
    #[arg(long)]
    pub out: PathBuf,
    /// system (tap only), call (mic + system) or in-person (mic only).
    #[arg(long, value_enum, default_value_t = Lanes::Call)]
    pub lanes: Lanes,
    /// Input device UID; default input otherwise.
    #[arg(long = "input-device")]
    pub input_device_uid: Option<String>,
}

impl CaptureSpike {
    async fn run(self) -> Outcome {
        if self.seconds <= 0.0 {
            return Err(Failure::usage("--seconds must be positive."));
        }
        let (mode, lane_override) = match self.lanes {
            Lanes::System => (
                steno_audio::CaptureMode::Call,
                Some(vec![AudioLane::System]),
            ),
            Lanes::Call => (steno_audio::CaptureMode::Call, None),
            Lanes::InPerson => (steno_audio::CaptureMode::InPerson, None),
        };
        let mut configuration = steno_audio::CaptureConfiguration::new(mode, &self.out);
        configuration
            .input_device_uid
            .clone_from(&self.input_device_uid);
        configuration.lane_override = lane_override;
        let session = super::record::session(
            configuration,
            super::record::Backend::Live,
            Some(self.seconds),
        )?;
        let threads = super::record::print_live(&session, false);
        let id = uuid::Uuid::new_v4();
        session
            .start(id)
            .map_err(|error| Failure::runtime(format!("could not start capture: {error}")))?;
        super::record::wait_for_stop(&session, Some(self.seconds)).await;
        let result = session.stop().map_err(Failure::runtime)?;
        drop(session);
        for thread in threads {
            let _ = thread.join();
        }
        println!(
            "master: {}",
            path_from_file_url(&result.asset.url)
                .unwrap_or_default()
                .display()
        );
        println!(
            "lanes: {}",
            result
                .asset
                .lanes
                .iter()
                .map(|l| l.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!("duration: {:.2} s", result.statistics.duration);
        for lane in &result.asset.lanes {
            if let Some(sidecar) = result.asset.sidecars_16k.get(lane)
                && let Some(path) = path_from_file_url(sidecar)
                && let Ok(samples) = WavFile::read_16k_mono(&path)
            {
                let onset = samples
                    .iter()
                    .position(|s| s.abs() > 0.01)
                    .map_or(-1.0, |i| {
                        #[allow(clippy::cast_precision_loss)]
                        let seconds = i as f64 / AudioBuffer16k::SAMPLE_RATE;
                        seconds
                    });
                println!("onset {}: {onset:.3} s", lane.as_str());
            }
        }
        Ok(())
    }
}

// models

#[derive(Debug, Args)]
pub struct ModelsOptions {
    #[arg(
        long = "models-dir",
        value_name = "DIR",
        help = "Models directory; defaults to the settings' directory, STENO_MODELS_DIR or Models in the support directory."
    )]
    pub models_directory: Option<PathBuf>,
    #[command(flatten)]
    pub database: DatabaseOptions,
}

impl ModelsOptions {
    fn service(&self) -> Result<ModelStoreSpeechModels, Failure> {
        Ok(ModelStoreSpeechModels::new(&self.directory()?))
    }

    /// The models directory; `dev models list` prints it.
    fn directory(&self) -> Result<PathBuf, Failure> {
        if let Some(directory) = &self.models_directory {
            return Ok(steno_services::speech::absolute(directory));
        }
        let store = self.database.open()?;
        let settings = store.settings().map_err(Failure::runtime)?;
        Ok(steno_services::speech::models_directory(
            &settings,
            &crate::wiring::paths()?,
        ))
    }
}

#[derive(Debug, Args)]
pub struct Models {
    #[command(subcommand)]
    pub command: ModelsCommand,
}

/// `parakeetV3`, `offlineDiarizer`, ...: the asset ids the Swift CLI took.
fn parse_asset(argument: &str) -> Result<ModelAsset, String> {
    argument.parse::<ModelAsset>().map_err(|_| {
        format!(
            "{argument} is not a model asset; one of {}.",
            ModelAsset::ALL
                .iter()
                .map(|a| a.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

#[derive(Debug, Subcommand)]
pub enum ModelsCommand {
    /// Show every asset and its state.
    List(ModelsOptions),
    /// Download an asset, with progress.
    Download {
        /// One of parakeetV3, parakeetUltra, parakeetDE, whisperLargeV3Turbo, offlineDiarizer.
        #[arg(value_parser = parse_asset)]
        asset: ModelAsset,
        #[command(flatten)]
        options: ModelsOptions,
    },
    /// Delete an asset's files.
    Remove {
        /// One of parakeetV3, parakeetUltra, parakeetDE, whisperLargeV3Turbo, offlineDiarizer.
        #[arg(value_parser = parse_asset)]
        asset: ModelAsset,
        #[command(flatten)]
        options: ModelsOptions,
    },
}

impl Models {
    fn run(self) -> Outcome {
        use steno_host::services::SpeechModels as _;
        match self.command {
            ModelsCommand::List(options) => {
                let service = options.service()?;
                println!("models: {}", options.directory()?.display());
                for asset in ModelAsset::ALL {
                    let line = match service.installed_size(*asset) {
                        Some(bytes) => {
                            format!("installed ({})", steno_host::labels::file_size(bytes))
                        }
                        None => format!(
                            "not installed (~{})",
                            steno_host::labels::file_size(asset.approximate_bytes())
                        ),
                    };
                    println!("{} {}: {line}", asset.as_str(), asset.display_name());
                }
                Ok(())
            }
            ModelsCommand::Download { asset, options } => {
                let service = options.service()?;
                let mut last = -1i64;
                service
                    .download(asset, &mut |fraction, phase| {
                        #[allow(clippy::cast_possible_truncation)]
                        let percent = (fraction * 100.0) as i64;
                        if percent != last {
                            last = percent;
                            eprintln!("{}: {percent}% {phase}", asset.as_str());
                        }
                    })
                    .map_err(Failure::runtime)?;
                println!("installed {}", asset.as_str());
                Ok(())
            }
            ModelsCommand::Remove { asset, options } => {
                options.service()?.remove(asset).map_err(Failure::runtime)?;
                println!("removed {}", asset.as_str());
                Ok(())
            }
        }
    }
}

// bakeoff

#[derive(Debug, Args)]
pub struct Bakeoff {
    /// A folder of recordings (WAV, CAF or m4a), one meeting lane each.
    pub audio_directory: PathBuf,
    /// Engines to compare.
    #[arg(long, value_delimiter = ',', default_value = "parakeet-v3")]
    pub engines: Vec<String>,
    #[arg(
        long = "reference-dir",
        help = "Reference transcripts; by default <name>.txt beside each recording."
    )]
    pub reference_directory: Option<PathBuf>,
    #[arg(
        long = "out",
        help = "Where the reports go; defaults to <audio-dir>/bakeoff."
    )]
    pub output: Option<PathBuf>,
    /// Also run the cleanup pass and report the cleaned WER.
    #[arg(long)]
    pub cleanup: bool,
    /// Print the report as JSON (the contents of report.json) instead of Markdown.
    #[arg(long)]
    pub json: bool,
    /// Use fake engines (one segment per second); for the tests.
    #[arg(long = "fake-engines", hide = true)]
    pub fake_engines: bool,
    #[command(flatten)]
    pub models: ModelsOptions,
}

#[derive(Debug, serde::Serialize)]
struct BakeoffRow {
    file: String,
    engine: String,
    audio_seconds: f64,
    wall_seconds: f64,
    segments: usize,
    wer: Option<f64>,
    cleaned_wer: Option<f64>,
    requests: i64,
    text: String,
}

impl Bakeoff {
    // One pass per engine and file, with the report at the end: the loop
    // body is the report row, split would scatter its columns.
    #[allow(clippy::too_many_lines)]
    async fn run(self) -> Outcome {
        for engine in &self.engines {
            if !crate::wiring::ENGINE_IDS.contains(&engine.as_str()) {
                return Err(Failure::usage(format!(
                    "error: invalid value '{engine}' for '--engines': expected one of {}",
                    crate::wiring::ENGINE_IDS.join(", ")
                )));
            }
        }
        if !self.audio_directory.is_dir() {
            return Err(Failure::usage(format!(
                "{} is not a directory",
                self.audio_directory.display()
            )));
        }
        let mut files: Vec<PathBuf> = std::fs::read_dir(&self.audio_directory)
            .map_err(Failure::runtime)?
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                matches!(
                    path.extension().and_then(|e| e.to_str()),
                    Some("wav" | "caf" | "m4a")
                )
            })
            .collect();
        files.sort();
        let output = self
            .output
            .clone()
            .unwrap_or_else(|| self.audio_directory.join("bakeoff"));
        std::fs::create_dir_all(&output).map_err(Failure::runtime)?;
        let decoder = steno_audio::SymphoniaAudioCodec::new();
        let cleaner: Option<Arc<dyn TranscriptCleaner>> = if self.cleanup {
            let store = self.models.database.open()?;
            let settings = store.settings().map_err(Failure::runtime)?;
            crate::wiring::llm_passes(&settings)
                .await?
                .map(|passes| passes.cleaner)
        } else {
            None
        };
        let mut rows = Vec::new();
        for engine_id in &self.engines {
            let engine: Arc<dyn SpeechEngine> = if self.fake_engines {
                Arc::new(steno_core::testing::FakeSpeechEngine {
                    id: engine_id.clone(),
                    ..steno_core::testing::FakeSpeechEngine::default()
                })
            } else {
                let mut settings = steno_core::Settings::default();
                settings.speech_engine_id.clone_from(engine_id);
                steno_services::speech::speech_engine(&settings, &self.models.directory()?)
            };
            engine.prepare().await.map_err(Failure::runtime)?;
            for file in &files {
                let name = file
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let buffer = decode_any(&decoder, file).await?;
                let started = std::time::Instant::now();
                let segments = engine
                    .transcribe(&buffer, None)
                    .await
                    .map_err(Failure::runtime)?;
                let wall = started.elapsed().as_secs_f64();
                let text = segments
                    .iter()
                    .map(|s| s.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                let reference = self.reference_for(file);
                let rate = reference
                    .as_deref()
                    .map(|reference| wer::word_errors(reference, &text).rate());
                let (cleaned_wer, requests) = match (&cleaner, &reference) {
                    (Some(cleaner), Some(reference)) => {
                        let transcript: Vec<_> = segments
                            .iter()
                            .enumerate()
                            .map(|(index, s)| steno_core::TranscriptSegment {
                                id: steno_core::derived_uuid(
                                    uuid::Uuid::nil(),
                                    &format!("bakeoff-{index}"),
                                ),
                                meeting_id: uuid::Uuid::nil(),
                                start: s.start,
                                end: s.end,
                                speaker_id: None,
                                lane: AudioLane::Mixed,
                                text: s.text.clone(),
                                raw_text: s.text.clone(),
                            })
                            .collect();
                        let output = cleaner
                            .clean(&steno_core::CleanupInput {
                                segments: transcript,
                                language: None,
                                participants: Vec::new(),
                                speakers: Vec::new(),
                                known_people: Vec::new(),
                            })
                            .await
                            .map_err(Failure::runtime)?;
                        let corrected_text = output
                            .segments
                            .iter()
                            .map(|s| s.text.as_str())
                            .collect::<Vec<_>>()
                            .join(" ");
                        (
                            Some(wer::word_errors(reference, &corrected_text).rate()),
                            output.usage.requests,
                        )
                    }
                    _ => (None, 0),
                };
                let stem = file
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                std::fs::write(
                    output.join(format!("{stem}.{engine_id}.json")),
                    serde_json::to_vec_pretty(&segments).map_err(Failure::runtime)?,
                )
                .map_err(Failure::runtime)?;
                rows.push(BakeoffRow {
                    file: name,
                    engine: engine_id.clone(),
                    audio_seconds: buffer.duration(),
                    wall_seconds: wall,
                    segments: segments.len(),
                    wer: rate,
                    cleaned_wer,
                    requests,
                    text,
                });
            }
        }
        let report = serde_json::json!({
            "generatedAt": steno_core::json::format_date(Utc::now()),
            "rows": rows,
        });
        std::fs::write(
            output.join("report.json"),
            serde_json::to_vec_pretty(&report).map_err(Failure::runtime)?,
        )
        .map_err(Failure::runtime)?;
        let markdown = render_bakeoff(&rows);
        std::fs::write(output.join("report.md"), &markdown).map_err(Failure::runtime)?;
        if self.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&report).map_err(Failure::runtime)?
            );
        } else {
            print!("{markdown}");
            println!("reports: {}", output.display());
        }
        Ok(())
    }

    fn reference_for(&self, file: &Path) -> Option<String> {
        let stem = file.file_stem()?;
        let directory = self
            .reference_directory
            .clone()
            .unwrap_or_else(|| self.audio_directory.clone());
        std::fs::read_to_string(directory.join(format!("{}.txt", stem.to_string_lossy()))).ok()
    }
}

fn percent(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_owned(), |v| format!("{:.1} %", v * 100.0))
}

fn render_bakeoff(rows: &[BakeoffRow]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    out.push_str(
        "| file | engine | audio s | wall s | RTFx | segments | WER | cleaned WER | requests |\n",
    );
    out.push_str("|---|---|---|---|---|---|---|---|---|\n");
    for row in rows {
        let rtfx = if row.wall_seconds > 0.0 {
            row.audio_seconds / row.wall_seconds
        } else {
            0.0
        };
        let _ = writeln!(
            out,
            "| {} | {} | {:.2} | {:.2} | {:.1} | {} | {} | {} | {} |",
            row.file,
            row.engine,
            row.audio_seconds,
            row.wall_seconds,
            rtfx,
            row.segments,
            percent(row.wer),
            percent(row.cleaned_wer),
            row.requests
        );
    }
    out.push_str("\n| engine | files | audio s | WER | cleaned WER | requests | failed |\n|---|---|---|---|---|---|---|\n");
    let mut engines: Vec<&str> = rows.iter().map(|r| r.engine.as_str()).collect();
    engines.dedup();
    for engine in engines {
        let mine: Vec<&BakeoffRow> = rows.iter().filter(|r| r.engine == engine).collect();
        let audio: f64 = mine.iter().map(|r| r.audio_seconds).sum();
        let mean = |pick: fn(&BakeoffRow) -> Option<f64>| {
            let values: Vec<f64> = mine.iter().filter_map(|r| pick(r)).collect();
            if values.is_empty() {
                None
            } else {
                #[allow(clippy::cast_precision_loss)]
                Some(values.iter().sum::<f64>() / values.len() as f64)
            }
        };
        let requests: i64 = mine.iter().map(|r| r.requests).sum();
        let _ = writeln!(
            out,
            "| {engine} | {} | {audio:.2} | {} | {} | {requests} | 0 |",
            mine.len(),
            percent(mean(|r| r.wer)),
            percent(mean(|r| r.cleaned_wer))
        );
    }
    out
}

async fn decode_any(
    decoder: &steno_audio::SymphoniaAudioCodec,
    file: &Path,
) -> Result<AudioBuffer16k, Failure> {
    use steno_core::AudioDecoder as _;
    let asset = steno_core::AudioAsset {
        id: uuid::Uuid::nil(),
        meeting_id: uuid::Uuid::nil(),
        url: steno_core::paths::file_url(file, false),
        format: match file.extension().and_then(|e| e.to_str()) {
            Some("caf") => steno_core::AudioFormat::Caf48kFloat32,
            Some("m4a") => steno_core::AudioFormat::M4aAac,
            _ => steno_core::AudioFormat::Wav16kInt16,
        },
        lanes: vec![AudioLane::Mixed],
        sidecars_16k: BTreeMap::new(),
        mixdown_url: None,
        retention: steno_core::AudioRetention::KeepForever,
        expires_at: None,
    };
    decoder
        .decode(&asset, AudioLane::Mixed)
        .await
        .map_err(|e| Failure::runtime(format!("{}: {e}", file.display())))
}

// diarize-sweep

#[derive(Debug, Args)]
pub struct DiarizeSweep {
    /// 16 kHz mono WAV files, one lane each (system.wav, mixed.wav or mic.wav).
    #[arg(required = true)]
    pub files: Vec<PathBuf>,
    /// Clustering thresholds to try.
    #[arg(long, value_delimiter = ',', default_value = "0.6,0.7,0.8,0.9,1.0")]
    pub thresholds: Vec<f32>,
    /// Cap the speaker count per file.
    #[arg(long = "max-speakers")]
    pub max_speakers: Option<usize>,
    /// Write the report with every cluster embedding as JSON.
    #[arg(long = "out")]
    pub output: Option<PathBuf>,
    /// Skip the refinement pass after the mapping.
    #[arg(long = "no-refinement")]
    pub no_refinement: bool,
    #[command(flatten)]
    pub models: ModelsOptions,
}

#[derive(Debug, serde::Serialize)]
struct SweepRun {
    file: String,
    threshold: f32,
    max_speakers: Option<usize>,
    audio_seconds: f64,
    wall_seconds: f64,
    clusters: Vec<SweepCluster>,
}

#[derive(Debug, serde::Serialize)]
struct SweepCluster {
    label: String,
    seconds: f64,
    confidence: f32,
    embedding: Option<Vec<f32>>,
}

impl DiarizeSweep {
    async fn run(self) -> Outcome {
        let store = steno_services::speech::speech_store_under(&self.models.directory()?);
        let mut runs = Vec::new();
        for threshold in &self.thresholds {
            let config = DiarizerConfig {
                clustering_threshold: *threshold,
                max_speakers: self.max_speakers,
                refines_clusters: !self.no_refinement,
                ..DiarizerConfig::default()
            };
            let diarizer = ModelDiarizer::onnx(
                config,
                steno_services::speech::diarize_store(&store),
                steno_services::speech::ONNX_THREADS,
            );
            for file in &self.files {
                let samples = WavFile::read_16k_mono(file)
                    .map_err(|e| Failure::runtime(format!("{}: {e}", file.display())))?;
                let buffer = AudioBuffer16k::new(samples);
                let started = std::time::Instant::now();
                let result = diarizer.diarize(&buffer).await.map_err(Failure::runtime)?;
                let wall = started.elapsed().as_secs_f64();
                let clusters: Vec<SweepCluster> = result
                    .clusters
                    .iter()
                    .map(|cluster| SweepCluster {
                        label: cluster.label.clone(),
                        seconds: cluster.ranges.iter().map(|r| r.upper - r.lower).sum(),
                        confidence: cluster.cluster_confidence,
                        embedding: cluster.embedding.as_ref().map(|e| e.0.clone()),
                    })
                    .collect();
                println!(
                    "{} @ {threshold:.2}: {} clusters in {wall:.2} s",
                    file.display(),
                    clusters.len()
                );
                for cluster in &clusters {
                    println!(
                        "  {} {:.1} s (confidence {:.2})",
                        cluster.label, cluster.seconds, cluster.confidence
                    );
                }
                runs.push(SweepRun {
                    file: file.display().to_string(),
                    threshold: *threshold,
                    max_speakers: self.max_speakers,
                    audio_seconds: buffer.duration(),
                    wall_seconds: wall,
                    clusters,
                });
            }
        }
        if let Some(output) = &self.output {
            std::fs::write(
                output,
                serde_json::to_vec_pretty(&serde_json::json!({ "runs": runs }))
                    .map_err(Failure::runtime)?,
            )
            .map_err(Failure::runtime)?;
            println!("report: {}", output.display());
        }
        Ok(())
    }
}

// llm

#[derive(Debug, Args)]
pub struct EndpointOptions {
    #[arg(
        long = "base-url",
        help = "OpenAI-compatible root, for example http://127.0.0.1:1234/v1; defaults to the settings."
    )]
    pub base_url: Option<String>,
    /// Model name as the server knows it.
    #[arg(long)]
    pub model: Option<String>,
    /// The model's context window.
    #[arg(long = "context-tokens")]
    pub context_tokens: Option<i64>,
    /// Use the Codex backend with this model and the Codex CLI's sign-in.
    #[arg(long = "codex-model")]
    pub codex_model: Option<String>,
    /// Ceiling for one answer.
    #[arg(long = "max-output-tokens")]
    pub max_output_tokens: Option<i64>,
    /// Per-attempt timeout in seconds.
    #[arg(long = "timeout")]
    pub timeout_seconds: Option<u64>,
    /// Log every request, retry and mode change to stderr.
    #[arg(long)]
    pub verbose: bool,
    #[command(flatten)]
    pub database: DatabaseOptions,
}

impl EndpointOptions {
    /// The endpoint from the flags over the stored settings.
    fn endpoint(&self) -> Result<LlmEndpoint, Failure> {
        let mut settings = self.database.open()?.settings().map_err(Failure::runtime)?;
        if let Some(model) = &self.codex_model {
            settings.llm_provider = steno_core::LlmProvider::Codex;
            settings.codex_model = Some(model.clone());
            settings.codex_confirmed_at = Some(Utc::now());
        } else if self.base_url.is_some() || self.model.is_some() {
            settings.llm_provider = steno_core::LlmProvider::Endpoint;
            if let Some(base) = &self.base_url {
                settings.llm_base_url = Some(base.clone());
            }
            if let Some(model) = &self.model {
                settings.llm_model = Some(model.clone());
            }
        }
        if let Some(tokens) = self.context_tokens {
            settings.llm_context_tokens = tokens;
            settings.codex_context_tokens = tokens;
        }
        let mut endpoint = LlmEndpoint::from_settings(&settings)
            .ok_or_else(|| Failure::usage("No LLM endpoint configured: pass --base-url and --model, --codex-model, or set them in Settings."))?;
        if let Some(max) = self.max_output_tokens {
            endpoint.max_output_tokens = max;
        }
        if let Some(seconds) = self.timeout_seconds {
            endpoint.request_timeout = std::time::Duration::from_secs(seconds);
        }
        Ok(endpoint)
    }

    async fn client(
        &self,
        retry: RetryPolicy,
    ) -> Result<(Arc<dyn LlmClient>, LlmEndpoint), Failure> {
        let endpoint = self.endpoint()?;
        let client = steno_services::llm::make_client(
            endpoint.clone(),
            crate::wiring::api_key().await?.as_deref(),
            &steno_services::llm::codex_store(),
            retry,
        );
        Ok((client, endpoint))
    }
}

#[derive(Debug, Args)]
pub struct Llm {
    #[command(subcommand)]
    pub command: LlmCommand,
}

#[derive(Debug, Subcommand)]
pub enum LlmCommand {
    /// GET /models and one tiny structured completion.
    Probe {
        /// Print the result as one JSON object.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        options: EndpointOptions,
    },
    /// Run the cleanup pass over a meeting.json and print the corrected segments.
    Cleanup {
        #[arg(help = "A meeting.json (a MeetingExport), for example from steno export.")]
        input: PathBuf,
        /// Write the export with cleaned segments to this path.
        #[arg(long)]
        out: Option<PathBuf>,
        #[command(flatten)]
        options: EndpointOptions,
    },
    /// Run the summary pass over a meeting.json and print the result.
    Summarize {
        #[arg(help = "A meeting.json (a MeetingExport), for example from steno export.")]
        input: PathBuf,
        /// Summary template id; defaults to the meeting's template.
        #[arg(long)]
        template: Option<String>,
        #[arg(long, help = "Print the SummaryOutput as JSON instead of text.")]
        json: bool,
        #[command(flatten)]
        options: EndpointOptions,
    },
}

fn load_export(path: &Path) -> Result<MeetingExport, Failure> {
    let bytes =
        std::fs::read(path).map_err(|e| Failure::usage(format!("{}: {e}", path.display())))?;
    serde_json::from_slice(&bytes).map_err(|e| Failure::usage(format!("{}: {e}", path.display())))
}

impl Llm {
    // Three subcommands sharing one endpoint and one client setup; the
    // text rendering of the summary is the bulk.
    #[allow(clippy::too_many_lines)]
    async fn run(self) -> Outcome {
        match self.command {
            LlmCommand::Probe { json, options } => {
                let (client, _) = options.client(RetryPolicy::with_max_attempts(1)).await?;
                let report = client
                    .probe()
                    .await
                    .map_err(|e| Failure::runtime(format!("probe failed: {e}")))?;
                let mode = serde_json::to_value(report.resolved_mode)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default();
                let millis = i64::try_from(report.round_trip.as_millis()).unwrap_or(i64::MAX);
                if json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "modelListed": report.model_listed,
                            "structuredOutput": mode,
                            "roundTripMilliseconds": millis,
                            "account": report.account_line,
                        })
                    );
                } else {
                    println!(
                        "model listed: {}",
                        match report.model_listed {
                            Some(true) => "yes",
                            Some(false) => "no",
                            None => "unknown (no model list)",
                        }
                    );
                    println!("structured output: {mode}");
                    println!("round trip: {millis} ms");
                    if let Some(account) = report.account_line {
                        println!("account: {account}");
                    }
                }
                Ok(())
            }
            LlmCommand::Cleanup {
                input,
                out,
                options,
            } => {
                let mut export = load_export(&input)?;
                let (client, endpoint) = options.client(RetryPolicy::default()).await?;
                let model: Arc<dyn steno_core::LanguageModel> = client;
                let cleaner = LlmTranscriptCleaner::new(model, endpoint);
                let output = cleaner
                    .clean(&steno_core::CleanupInput {
                        segments: export.segments.clone(),
                        language: export.meeting.language.clone(),
                        participants: export.participants.clone(),
                        speakers: export.speakers.clone(),
                        known_people: export.persons.clone(),
                    })
                    .await
                    .map_err(Failure::runtime)?;
                let mut changed = 0;
                for (original, cleaned) in export.segments.iter_mut().zip(&output.segments) {
                    if original.text != cleaned.text {
                        changed += 1;
                        println!(
                            "{}\n  raw:     {}\n  cleaned: {}",
                            original.id, original.raw_text, cleaned.text
                        );
                        original.text.clone_from(&cleaned.text);
                    }
                }
                println!("{changed} of {} segments changed", export.segments.len());
                println!(
                    "usage: {} request(s), {} prompt + {} completion tokens",
                    output.usage.requests,
                    output.usage.prompt_tokens,
                    output.usage.completion_tokens
                );
                if let Some(out) = out {
                    let json = steno_adapters::ArtifactRenderer
                        .render_json(&export)
                        .map_err(Failure::runtime)?;
                    std::fs::write(&out, json).map_err(Failure::runtime)?;
                    println!("wrote {}", out.display());
                }
                Ok(())
            }
            LlmCommand::Summarize {
                input,
                template,
                json,
                options,
            } => {
                let export = load_export(&input)?;
                let template_id = template.unwrap_or_else(|| export.meeting.template_id.clone());
                let template = SummaryTemplate::bundled_with_id(&template_id).ok_or_else(|| {
                    Failure::usage(format!(
                        "Unknown template {template_id}. Bundled: {}.",
                        SummaryTemplate::BUNDLED_IDS.join(", ")
                    ))
                })?;
                let (client, endpoint) = options.client(RetryPolicy::default()).await?;
                let model: Arc<dyn steno_core::LanguageModel> = client;
                let summarizer = LlmMeetingSummarizer::new(
                    model,
                    endpoint,
                    steno_adapters::runtime::local_time_zone(),
                );
                let output = summarizer
                    .summarize(&steno_core::SummaryInput {
                        meeting: export.meeting.clone(),
                        segments: export.segments.clone(),
                        speakers: export.speakers.clone(),
                        participants: export.participants.clone(),
                        known_people: export.persons.clone(),
                        template: template.clone(),
                    })
                    .await
                    .map_err(Failure::runtime)?;
                if json {
                    let mut value = serde_json::json!({
                        "title": output.title,
                        "summary": output.summary,
                        "decisions": output.decisions,
                        "tasks": output.tasks,
                        "speakerNames": output.speaker_names,
                        "language": output.language,
                        "usage": output.usage,
                    });
                    if let Some(object) = value.as_object_mut() {
                        object.retain(|_, v| !v.is_null());
                    }
                    println!(
                        "{}",
                        steno_bridge::json::to_canonical_string(&value)
                            .map_err(Failure::runtime)?
                    );
                } else {
                    println!("# {}\n", output.title);
                    for section in &output.summary.sections {
                        println!("## {}\n", section.heading);
                        for bullet in &section.bullets {
                            if bullet.lead.is_empty() {
                                println!("- {}", bullet.text);
                            } else {
                                println!("- **{}** {}", bullet.lead, bullet.text);
                            }
                        }
                        println!();
                    }
                    if !output.decisions.is_empty() {
                        println!("## Decisions\n");
                        for decision in &output.decisions {
                            println!("- {decision}");
                        }
                        println!();
                    }
                    if !output.tasks.is_empty() {
                        println!("## Tasks\n");
                        for task in &output.tasks {
                            match &task.assignee_name {
                                Some(name) => println!("- [ ] {} ({name})", task.text),
                                None => println!("- [ ] {}", task.text),
                            }
                        }
                        println!();
                    }
                    let named: Vec<&steno_core::SpeakerNameSuggestion> = output
                        .speaker_names
                        .iter()
                        .filter(|s| s.name.is_some())
                        .collect();
                    if !named.is_empty() {
                        println!("## Speaker names\n");
                        for suggestion in named {
                            let label = export
                                .speakers
                                .iter()
                                .find(|s| s.id == suggestion.speaker_id)
                                .map_or_else(
                                    || suggestion.speaker_id.to_string(),
                                    |s| s.cluster_label.clone(),
                                );
                            println!(
                                "- {label} → {} ({:.1})",
                                suggestion.name.as_deref().unwrap_or_default(),
                                suggestion.confidence
                            );
                        }
                        println!();
                    }
                    println!(
                        "usage: {} request(s), {} prompt + {} completion tokens",
                        output.usage.requests,
                        output.usage.prompt_tokens,
                        output.usage.completion_tokens
                    );
                }
                Ok(())
            }
        }
    }
}

// handover

#[derive(Debug, Args)]
pub struct Handover {
    #[command(subcommand)]
    pub command: HandoverCommand,
}

#[derive(Debug, Subcommand)]
pub enum HandoverCommand {
    /// Advertise the handover service and accept phone uploads.
    Serve {
        /// Open a pairing window at once and print the QR payload URL.
        #[arg(long)]
        pair: bool,
        /// Bonjour service name shown on the phone.
        #[arg(long)]
        name: Option<String>,
        /// Listen on this port; 0 lets the system choose.
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Directory for partial uploads; a temp directory by default.
        #[arg(long)]
        inbox: Option<PathBuf>,
    },
}

impl Handover {
    async fn run(self) -> Outcome {
        let HandoverCommand::Serve {
            pair,
            name,
            port,
            inbox,
        } = self.command;
        let name = name.unwrap_or_else(steno_handover::HandoverConfiguration::default_service_name);
        let inbox = inbox.unwrap_or_else(|| {
            std::env::temp_dir().join(format!("steno-handover-{}", uuid::Uuid::new_v4()))
        });
        let store = Arc::new(steno_core::Store::in_memory().map_err(Failure::runtime)?);
        let intake = Arc::new(steno_core::testing::FakeHandoverIntake::default());
        let identity =
            steno_handover::HandoverIdentity::mint(&format!("Steno on {name}"), Utc::now())
                .map_err(Failure::runtime)?;
        let mac_id = identity.mac_id();
        let fingerprint = identity.fingerprint();
        let configuration = steno_handover::HandoverConfiguration {
            service_name: name.clone(),
            advertise: true,
            inbox_directory: inbox,
            port,
            ..steno_handover::HandoverConfiguration::default()
        };
        let pairing_window = configuration.pairing_window;
        let service = steno_handover::HandoverService::with_wall_clock(
            configuration,
            store,
            intake,
            Arc::new(identity),
        );
        service.start().await.map_err(Failure::runtime)?;
        let steno_handover::ListenerState::Listening { port: bound } = service.state() else {
            return Err(Failure::runtime(
                "the handover listener is not listening after start",
            ));
        };
        println!("Steno handover listening on port {bound}");
        println!("Mac id: {}", steno_core::json::uuid_string(mac_id));
        println!(
            "Fingerprint (hex): {}",
            steno_handover::identity::hex(&fingerprint)
        );
        println!("Advertising _steno._tcp as \"{name}\"");
        if pair {
            let payload = service.begin_pairing();
            println!(
                "\nPairing window open for {} minutes. Scan this on the phone:",
                pairing_window.as_secs() / 60
            );
            println!("{}", payload.url_string());
            print_qr(&payload.url_string());
        } else {
            println!("\nRun with --pair to open a pairing window, or pair from the app.");
        }
        println!("\nPress Ctrl-C to stop.");
        let _ = tokio::signal::ctrl_c().await;
        println!("\nStopping.");
        service.stop().await;
        Ok(())
    }
}

/// Renders the payload as a QR with `qrencode` when it is on PATH; the
/// app draws the real QR.
fn print_qr(text: &str) {
    let hint = "(install qrencode to render a scannable QR in the terminal)";
    match std::process::Command::new("qrencode")
        .args(["-t", "ANSIUTF8", "-m", "1", text])
        .status()
    {
        Ok(status) if status.success() => {}
        _ => println!("{hint}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bakeoff_table_formats_percentages_and_dashes() {
        let rows = vec![BakeoffRow {
            file: "tone-1s.wav".into(),
            engine: "parakeet-v3".into(),
            audio_seconds: 1.0,
            wall_seconds: 0.5,
            segments: 1,
            wer: Some(1.0 / 3.0),
            cleaned_wer: None,
            requests: 0,
            text: String::new(),
        }];
        let table = render_bakeoff(&rows);
        assert!(
            table
                .contains("| tone-1s.wav | parakeet-v3 | 1.00 | 0.50 | 2.0 | 1 | 33.3 % | - | 0 |")
        );
        assert!(table.contains("| parakeet-v3 | 1 | 1.00 | 33.3 % | - | 0 | 0 |"));
    }
}
