//! `steno record --mode call|in-person --out DIR [--seconds N] [--backend
//! live|synthetic] [--keep-raw-mic]`: records one meeting folder under DIR,
//! prints the lane levels at 10 Hz to stderr and the files plus statistics
//! at the end; `--keep-raw-mic` keeps the microphone before echo
//! cancellation beside the master as `mic.raw.caf`.
//! Stops after `--seconds` or on Ctrl-C. `--backend synthetic` needs no
//! devices. Swift: `Sources/steno/Commands/Record.swift`.

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Args, ValueEnum};
use steno_audio::testing::SyntheticCaptureBackend;
use steno_audio::testing::synthetic::SyntheticOptions;
use steno_audio::{
    CaptureBackend, CaptureConfiguration, CaptureMode, CaptureNotice, CaptureSession, CaptureState,
    DeviceChangeReason, LaneLevels, LiveCaptureBackend, SpeexEchoCanceller, SystemClock,
};
use steno_core::{AudioLane, EchoCanceller, RecordingLayout, paths::file_url_path};
use uuid::Uuid;

use crate::wiring::{Failure, Outcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    Call,
    InPerson,
}

impl From<Mode> for CaptureMode {
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::Call => CaptureMode::Call,
            Mode::InPerson => CaptureMode::InPerson,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Backend {
    Live,
    Synthetic,
}

#[derive(Debug, Args)]
pub struct Record {
    /// call (mic + system lanes) or in-person (one room lane).
    #[arg(long, value_enum, default_value_t = Mode::Call)]
    pub mode: Mode,
    /// The audio folder; the meeting folder is created inside it.
    #[arg(long)]
    pub out: PathBuf,
    /// Stop after this many seconds; otherwise until Ctrl-C.
    #[arg(long)]
    pub seconds: Option<f64>,
    /// live (the platform's capture) or synthetic (tones, no devices).
    #[arg(long, value_enum, default_value_t = Backend::Live)]
    pub backend: Backend,
    /// Input device UID; default input otherwise.
    #[arg(long = "input-device")]
    pub input_device_uid: Option<String>,
    /// Meeting id for the folder name; random otherwise.
    #[arg(long = "meeting-id")]
    pub meeting_id: Option<String>,
    /// Disable echo cancellation in call mode.
    #[arg(long = "no-aec")]
    pub no_echo_cancellation: bool,
    /// Also write mic.raw.caf (before echo cancellation).
    #[arg(long = "keep-raw-mic")]
    pub keep_raw_mic: bool,
    /// Do not print levels.
    #[arg(long)]
    pub quiet: bool,
}

/// One line for stderr, shared with `steno dev capture-spike`.
#[must_use]
pub fn notice_line(notice: &CaptureNotice) -> String {
    match notice {
        CaptureNotice::DeviceChanged(reason) => {
            format!("{}; reconnecting", reason_words(*reason))
        }
        CaptureNotice::StillRestarting { attempt } => {
            format!(
                "no audio after {attempt} restarts; still trying until audio arrives or you stop"
            )
        }
        CaptureNotice::Delivering => "audio is arriving again".to_owned(),
        CaptureNotice::DeviceResumed {
            attempt,
            gap_seconds,
        } => {
            format!("device resumed: attempt {attempt}, gap {gap_seconds:.2} s")
        }
    }
}

/// What a device change means, in plain words.
fn reason_words(reason: DeviceChangeReason) -> &'static str {
    match reason {
        DeviceChangeReason::DefaultOutputChanged => "the default output moved",
        DeviceChangeReason::DefaultInputChanged => "the default microphone changed",
        DeviceChangeReason::OutputDeviceGone => "the output device is gone",
        DeviceChangeReason::InputDeviceGone => "the microphone is gone",
        DeviceChangeReason::SampleRateChanged => "the devices' sample rate changed",
        DeviceChangeReason::DeliveryStalled => "no audio arrives from the devices",
        DeviceChangeReason::AudioServiceRestarted => "the audio service restarted",
        DeviceChangeReason::ChosenInputRecheck => "the chosen microphone is back",
    }
}

/// One line per level update.
#[must_use]
pub fn level_line(levels: &LaneLevels) -> String {
    match &levels.system {
        Some(system) => format!(
            "mic {:6.1} dBFS  system {:6.1} dBFS",
            levels.mic.rms, system.rms
        ),
        None => format!("mic {:6.1} dBFS", levels.mic.rms),
    }
}

/// Forwards every level and notice to stderr on threads that end with the
/// session.
pub fn print_live(session: &CaptureSession, quiet: bool) -> Vec<std::thread::JoinHandle<()>> {
    let mut threads = Vec::new();
    if !quiet {
        let levels = session.levels();
        threads.push(std::thread::spawn(move || {
            while let Ok(update) = levels.recv() {
                eprintln!("{}", level_line(&update));
            }
        }));
    }
    let notices = session.notices();
    threads.push(std::thread::spawn(move || {
        while let Ok(notice) = notices.recv() {
            eprintln!("{}", notice_line(&notice));
        }
    }));
    threads
}

/// Builds the session for the flags: the live backend, or synthetic tones
/// paced to wall time.
pub fn session(
    configuration: CaptureConfiguration,
    backend: Backend,
    seconds: Option<f64>,
) -> Result<CaptureSession, Failure> {
    let lanes = configuration.lanes();
    let capture: Arc<dyn CaptureBackend> = match backend {
        Backend::Live => Arc::new(LiveCaptureBackend::new()),
        Backend::Synthetic => {
            let mut options = SyntheticOptions::tones(
                &lanes,
                &[
                    (AudioLane::Mic, 440.0),
                    (AudioLane::System, 1_000.0),
                    (AudioLane::Mixed, 440.0),
                ],
                seconds.unwrap_or(3_600.0),
            );
            options.real_time = true;
            Arc::new(SyntheticCaptureBackend::new(options))
        }
    };
    let echo: Option<Box<dyn EchoCanceller>> = if configuration.uses_echo_cancellation() {
        Some(Box::new(
            SpeexEchoCanceller::new(steno_audio::SAMPLE_RATE, steno_audio::FRAME_SIZE)
                .map_err(Failure::runtime)?,
        ))
    } else {
        None
    };
    CaptureSession::with_backend(
        configuration,
        capture,
        echo,
        CaptureSession::DEFAULT_WRITER_HEADROOM_FRAMES,
        Arc::new(SystemClock::new()),
    )
    .map_err(Failure::runtime)
}

/// Waits for `--seconds`, Ctrl-C or a failed state, whichever comes first.
pub async fn wait_for_stop(session: &CaptureSession, seconds: Option<f64>) {
    let states = session.states();
    let (failed_tx, mut failed_rx) = tokio::sync::oneshot::channel::<()>();
    std::thread::spawn(move || {
        while let Ok(state) = states.recv() {
            if let CaptureState::Failed { error, .. } = state {
                eprintln!("capture failed: {error}");
                let _ = failed_tx.send(());
                return;
            }
        }
    });
    let deadline = async {
        match seconds {
            Some(seconds) => tokio::time::sleep(std::time::Duration::from_secs_f64(seconds)).await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        () = deadline => {}
        _ = tokio::signal::ctrl_c() => {}
        _ = &mut failed_rx => {}
    }
}

impl Record {
    fn validate(&self) -> Result<Uuid, Failure> {
        let id = match &self.meeting_id {
            Some(text) => {
                Uuid::parse_str(text).map_err(|_| Failure::usage("--meeting-id must be a UUID."))?
            }
            None => Uuid::new_v4(),
        };
        if let Some(seconds) = self.seconds
            && seconds <= 0.0
        {
            return Err(Failure::usage("--seconds must be positive."));
        }
        Ok(id)
    }

    pub async fn run(self) -> Outcome {
        let id = self.validate()?;
        let out = crate::wiring::standardized(&self.out);
        let mut configuration = CaptureConfiguration::new(self.mode.into(), &out);
        configuration
            .input_device_uid
            .clone_from(&self.input_device_uid);
        configuration.echo_cancellation = !self.no_echo_cancellation;
        configuration.keep_raw_mic_lane = self.keep_raw_mic;
        let session = session(configuration, self.backend, self.seconds)?;
        let threads = print_live(&session, self.quiet);
        session
            .start(id)
            .map_err(|error| Failure::runtime(format!("could not start recording: {error}")))?;
        eprintln!(
            "recording {} into {} ({})",
            steno_core::json::uuid_string(id),
            out.display(),
            match self.mode {
                Mode::Call => "call",
                Mode::InPerson => "in-person",
            }
        );
        wait_for_stop(&session, self.seconds).await;
        let result = session
            .stop()
            .map_err(|error| Failure::runtime(format!("could not stop recording: {error}")))?;
        let failed = session.state().failure().cloned();
        drop(session);
        for thread in threads {
            let _ = thread.join();
        }
        println!("meeting: {}", steno_core::json::uuid_string(id));
        println!(
            "master: {}",
            file_url_path(&result.asset.url)
                .unwrap_or_default()
                .display()
        );
        for lane in &result.asset.lanes {
            if let Some(sidecar) = result.asset.sidecars_16k.get(lane) {
                println!(
                    "sidecar {}: {}",
                    lane.as_str(),
                    file_url_path(sidecar).unwrap_or_default().display()
                );
            }
        }
        if let Some(raw) = RecordingLayout::from_asset(&result.asset)
            .map(|layout| layout.directory.join("mic.raw.caf"))
            .filter(|raw| self.keep_raw_mic && raw.is_file())
        {
            println!("raw mic: {}", raw.display());
        }
        println!("duration: {:.2} s", result.statistics.duration);
        let dropped: Vec<String> = result
            .statistics
            .dropped_frames
            .iter()
            .map(|(lane, count)| format!("{}={count}", lane.as_str()))
            .collect();
        println!(
            "dropped frames: {}",
            if dropped.is_empty() {
                "none".to_owned()
            } else {
                dropped.join(" ")
            }
        );
        println!(
            "system lane silent: {}",
            result.statistics.system_lane_silent
        );
        println!("device changes: {}", result.statistics.device_changes);
        println!("gap filled: {:.2} s", result.statistics.gap_seconds);
        println!(
            "ended on device loss: {}",
            result.statistics.ended_on_device_loss
        );
        if let Some(error) = failed {
            return Err(Failure::runtime(format!("recording ended with {error}")));
        }
        Ok(())
    }
}
