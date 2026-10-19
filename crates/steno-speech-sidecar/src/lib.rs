//! `steno-speech-sidecar`: the child process that runs Parakeet and the
//! diarizer on ONNX Runtime for the app, driven through
//! [`steno_speech::SidecarSpeechEngine`] over
//! [`steno_speech::sidecar::protocol`]. It gets the audio only over stdin,
//! opens no connection and never downloads; ONNX Runtime's telemetry is off for
//! every session. It exits as soon as stdin ends or stdout breaks, and on unix
//! ignores SIGINT, SIGTERM and SIGHUP so a job ends only when the parent does.
//!
//! `--fake-engine` replaces the models with an engine that describes the audio
//! it received; with it, `--fault <kind>` ([`Fault`]) and `--fault-once <path>`
//! drive the isolation tests.

use std::fs::File;
use std::io::{self, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use steno_core::{AudioBuffer16k, Embedding, LanguageTag, RawSegment, SpeakerCluster, TimeRange};
use steno_diarize::models::ModelPaths;
use steno_diarize::{DiarizerConfig, Pipeline};
use steno_speech::engine::pipeline_config;
use steno_speech::sidecar::FALLBACK_NOTICE;
use steno_speech::sidecar::protocol::{
    self, DiarizedCluster, MAX_HEADER_BYTES, PROTOCOL_VERSION, Reply, Request,
};
use steno_speech::{
    EncoderProvider, ModelStore, OnnxBackend, OnnxOptions, OnnxSpeechEngine, Transcriber, VadConfig,
};

steno_core::string_enum! {
    /// What the fake engine does instead of answering the next transcription.
    /// The crashes, the hang, the allocation and the error apply to the next
    /// diarization too; the `*Diarizing` and `*DiarizerLoad` faults only to
    /// that step.
    pub enum Fault {
        /// `std::process::abort`, the way an uncaught C++ exception in ONNX
        /// Runtime ends the process.
        Abort = "abort",
        /// A line of stderr that is not UTF-8, then a Rust panic (status 101).
        Panic = "panic",
        /// 1 MiB of stderr without a newline, then a Rust panic: the parent's
        /// crash report must stay small.
        Flood = "flood",
        /// Exits with status 3.
        Exit = "exit",
        /// Never answers.
        Hang = "hang",
        /// Allocates and touches 4 GiB in 16 MiB steps, then hangs.
        Allocate = "allocate",
        /// Writes bytes that are not a frame, then hangs.
        Garbage = "garbage",
        /// Answers with an error and keeps running.
        Error = "error",
        /// On unix, leaves a process behind that holds the child's stderr and
        /// writes a line 300 ms after the child exits with status 3. The crash
        /// report must still hold that line.
        LateStderr = "late-stderr",
        /// Answers, then reports the CPU as the encoder's provider and writes
        /// why to stderr, as after a failed `DirectML` run.
        Fallback = "fallback",
        /// Fails and leaves the engine without an encoder, as when the CPU
        /// cannot reopen the model after a failed `DirectML` run.
        LoseEncoder = "lose-encoder",
        /// `std::process::abort` inside any load.
        AbortOnLoad = "abort-on-load",
        /// `std::process::abort` inside a load that asks for `DirectML`, as a
        /// driver may end the probe.
        AbortOnDirectmlLoad = "abort-on-directml-load",
        /// Answers with a transcript over the frame limit.
        Oversize = "oversize",
        /// Answers, then reports a resident set over any ceiling from the next
        /// heartbeat on.
        Swell = "swell",
        /// At start: sends nothing, reads nothing, hangs.
        Silent = "silent",
        /// At start: announces protocol version 0 in its ready message.
        WrongProtocol = "wrong-protocol",
        /// At start: waits 200 ms before its ready message.
        SlowStart = "slow-start",
        /// `std::process::abort` inside the next diarization; transcriptions
        /// answer.
        AbortDiarizing = "abort-diarizing",
        /// Refuses every diarizer load with an error and keeps running;
        /// transcriptions answer.
        RefuseDiarizer = "refuse-diarizer",
        /// `std::process::abort` inside the next diarizer load; transcriptions
        /// answer.
        AbortOnDiarizerLoad = "abort-on-diarizer-load",
        /// Never answers the next diarizer load; transcriptions answer.
        HangOnDiarizerLoad = "hang-on-diarizer-load",
    }
}

impl Fault {
    /// Committed when the child starts, not at the next transcription.
    fn at_start(self) -> bool {
        matches!(
            self,
            Fault::Silent | Fault::WrongProtocol | Fault::SlowStart
        )
    }
}

/// Writes one line to stderr, dropping it when stderr is gone, where
/// `eprintln!` would panic.
macro_rules! stderr_line {
    ($($line:tt)*) => {{
        let _ = writeln!(io::stderr(), $($line)*);
    }};
}

/// The command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// `--fake-engine`: no models, an answer that describes the audio.
    pub fake_engine: bool,
    /// `--fault <kind>`, with the fake engine only.
    pub fault: Option<Fault>,
    /// `--fault-once <path>`: only the child that creates `path` faults.
    pub fault_once: Option<PathBuf>,
    /// `--heartbeat-ms <n>`: how often the resident set is reported.
    pub heartbeat: Duration,
}

impl Options {
    /// Parses the command line; faults need the fake engine.
    pub fn parse(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<Self, String> {
        let mut options = Options {
            fake_engine: false,
            fault: None,
            fault_once: None,
            heartbeat: Duration::from_millis(200),
        };
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let mut value = |name: &str| {
                args.next()
                    .ok_or_else(|| format!("{name} needs a value"))
                    .map(|v| v.to_string_lossy().into_owned())
            };
            match arg.to_string_lossy().as_ref() {
                "--fake-engine" => options.fake_engine = true,
                "--fault" => {
                    let name = value("--fault")?;
                    options.fault =
                        Some(name.parse().map_err(|_| format!("unknown fault {name}"))?);
                }
                "--fault-once" => options.fault_once = Some(PathBuf::from(value("--fault-once")?)),
                "--heartbeat-ms" => {
                    let ms: u64 = value("--heartbeat-ms")?
                        .parse()
                        .map_err(|e| format!("--heartbeat-ms: {e}"))?;
                    options.heartbeat = Duration::from_millis(ms.max(1));
                }
                other => return Err(format!("unknown argument {other}")),
            }
        }
        if options.fault.is_some() && !options.fake_engine {
            return Err("--fault needs --fake-engine".to_owned());
        }
        Ok(options)
    }
}

/// What the child runs requests on.
trait Engine {
    /// Loads the models once; returns where the encoder runs.
    fn load(
        &mut self,
        models_root: &Path,
        options: &OnnxOptions,
    ) -> Result<EncoderProvider, String>;
    fn loaded(&self) -> bool;
    /// Where the encoder runs now; `None` before a load or when unknown.
    fn provider(&self) -> Option<EncoderProvider>;
    /// Why the encoder is on the CPU though the load asked for `DirectML`
    /// ([`OnnxBackend::fallback`]).
    fn fallback(&self) -> Option<&'static str> {
        None
    }
    /// `false` once a loaded encoder lost its session
    /// ([`OnnxBackend::usable`]); the child then exits rather than fail every
    /// request.
    fn usable(&self) -> bool {
        true
    }
    fn transcribe(
        &mut self,
        samples: &[f32],
        hint: Option<&LanguageTag>,
    ) -> Result<Vec<RawSegment>, String>;
    /// Loads the diarizer's two models, replacing any loaded before.
    fn load_diarizer(&mut self, models: &ModelPaths, threads: usize) -> Result<(), String>;
    /// Diarizes `audio` with the diarizer's default configuration.
    fn diarize(&mut self, audio: AudioBuffer16k) -> Result<Vec<SpeakerCluster>, String>;
}

/// Parakeet and the diarizer on ONNX Runtime from installed models.
#[derive(Default)]
struct OnnxEngine {
    transcriber: Option<Transcriber<OnnxBackend>>,
    diarizer: Option<Pipeline<steno_diarize::onnx::OnnxBackend>>,
}

impl Engine for OnnxEngine {
    fn load(
        &mut self,
        models_root: &Path,
        options: &OnnxOptions,
    ) -> Result<EncoderProvider, String> {
        let transcriber = match &mut self.transcriber {
            Some(transcriber) => transcriber,
            None => self.transcriber.insert(
                OnnxSpeechEngine::load_installed(
                    &ModelStore::new(models_root),
                    options,
                    pipeline_config(),
                    VadConfig::default(),
                )
                .map_err(|e| e.to_string())?,
            ),
        };
        Ok(transcriber.backend().provider())
    }

    fn loaded(&self) -> bool {
        self.transcriber.is_some()
    }

    fn provider(&self) -> Option<EncoderProvider> {
        Some(self.transcriber.as_ref()?.backend().provider())
    }

    fn fallback(&self) -> Option<&'static str> {
        self.transcriber.as_ref()?.backend().fallback()
    }

    fn usable(&self) -> bool {
        self.transcriber
            .as_ref()
            .is_none_or(|transcriber| transcriber.backend().usable())
    }

    fn transcribe(
        &mut self,
        samples: &[f32],
        hint: Option<&LanguageTag>,
    ) -> Result<Vec<RawSegment>, String> {
        let transcriber = self
            .transcriber
            .as_mut()
            .ok_or_else(|| "the models are not loaded".to_owned())?;
        transcriber
            .transcribe(samples, hint)
            .map(|t| t.segments)
            .map_err(|e| e.to_string())
    }

    fn load_diarizer(&mut self, models: &ModelPaths, threads: usize) -> Result<(), String> {
        self.diarizer = None;
        let backend =
            steno_diarize::onnx::OnnxBackend::load(models, threads).map_err(|e| e.to_string())?;
        self.diarizer = Some(Pipeline::new(backend, DiarizerConfig::default()));
        Ok(())
    }

    fn diarize(&mut self, audio: AudioBuffer16k) -> Result<Vec<SpeakerCluster>, String> {
        let pipeline = self
            .diarizer
            .as_mut()
            .ok_or_else(|| "the diarizer's models are not loaded".to_owned())?;
        pipeline
            .diarize(&audio)
            .map(|result| result.clusters)
            .map_err(|e| e.to_string())
    }
}

/// No models: one segment naming what arrived, or the configured fault.
struct FakeEngine {
    loaded: bool,
    fault: Option<Fault>,
    fault_once: Option<PathBuf>,
    /// Set by [`Fault::Fallback`].
    fell_back: bool,
    /// Set by [`Fault::LoseEncoder`].
    lost_encoder: bool,
}

impl FakeEngine {
    /// The fault to commit now: always, or with `--fault-once` only in the
    /// child that creates the marker.
    fn fault_now(&self) -> Option<Fault> {
        let fault = self.fault?;
        match &self.fault_once {
            Some(marker) => File::options()
                .write(true)
                .create_new(true)
                .open(marker)
                .ok()
                .map(|mut file| {
                    let _ = write!(file, "{}", std::process::id());
                    fault
                }),
            None => Some(fault),
        }
    }
}

impl Engine for FakeEngine {
    /// Reports `DirectML` when asked for it, as if the probe had passed; aborts
    /// instead with [`Fault::AbortOnLoad`], or [`Fault::AbortOnDirectmlLoad`]
    /// when asked for `DirectML`.
    fn load(&mut self, _: &Path, options: &OnnxOptions) -> Result<EncoderProvider, String> {
        let aborts = match self.fault {
            Some(Fault::AbortOnLoad) => true,
            Some(Fault::AbortOnDirectmlLoad) => options.directml,
            _ => false,
        };
        if aborts && self.fault_now().is_some() {
            std::process::abort();
        }
        self.loaded = true;
        Ok(if options.directml {
            EncoderProvider::DirectMl
        } else {
            EncoderProvider::Cpu
        })
    }

    fn loaded(&self) -> bool {
        self.loaded
    }

    /// `None` until [`Fault::Fallback`].
    fn provider(&self) -> Option<EncoderProvider> {
        self.fell_back.then_some(EncoderProvider::Cpu)
    }

    /// Fixed words of its own after [`Fault::Fallback`].
    fn fallback(&self) -> Option<&'static str> {
        self.fell_back.then_some("the fake engine fell back")
    }

    /// `false` after [`Fault::LoseEncoder`].
    fn usable(&self) -> bool {
        !self.lost_encoder
    }

    fn transcribe(
        &mut self,
        samples: &[f32],
        hint: Option<&LanguageTag>,
    ) -> Result<Vec<RawSegment>, String> {
        // Left for the diarization or diarizer load it is meant for.
        let fault = if matches!(
            self.fault,
            Some(Fault::AbortDiarizing | Fault::AbortOnDiarizerLoad | Fault::HangOnDiarizerLoad)
        ) {
            None
        } else {
            self.fault_now()
        };
        match fault {
            Some(Fault::Abort) => std::process::abort(),
            Some(Fault::Panic) => {
                // A non-UTF-8 line first: the parent must still keep the panic
                // message.
                let _ = io::stderr().write_all(b"native noise \xff\xfe\n");
                panic!("simulated panic in the speech engine")
            }
            Some(Fault::Flood) => {
                let _ = io::stderr().write_all(&vec![b'x'; 1 << 20]);
                panic!("simulated panic after a flood of stderr")
            }
            Some(Fault::Exit) => std::process::exit(3),
            Some(Fault::LateStderr) => {
                // Only stderr is inherited, so the parent sees stdout close at
                // once.
                #[cfg(unix)]
                let _ = std::process::Command::new("sh")
                    .args(["-c", "sleep 0.3; echo 'a line after the exit' >&2"])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .spawn();
                std::process::exit(3)
            }
            Some(Fault::Hang) => hang(),
            Some(Fault::Allocate) => allocate_and_hang(),
            Some(Fault::Garbage) => {
                let mut out = io::stdout().lock();
                // Under the header limit, so only the first byte after it shows
                // this is no frame.
                let _ = out.write_all(&(16u32 << 20).to_le_bytes());
                let _ = out.write_all(b"this is not a frame");
                let _ = out.flush();
                drop(out);
                hang()
            }
            Some(Fault::Error) => Err("simulated failure in the speech engine".to_owned()),
            Some(Fault::Fallback) => {
                self.fell_back = true;
                Ok(describe(samples, hint))
            }
            Some(Fault::LoseEncoder) => {
                self.lost_encoder = true;
                Err("simulated loss of the speech encoder".to_owned())
            }
            Some(Fault::Oversize) => {
                let mut segments = describe(samples, hint);
                segments[0].text = "x".repeat(MAX_HEADER_BYTES as usize);
                Ok(segments)
            }
            Some(Fault::Swell) => {
                SWELL_AFTER_REPLY.store(true, Ordering::Relaxed);
                Ok(describe(samples, hint))
            }
            // The start and load faults were committed, if at all, there.
            Some(
                Fault::Silent
                | Fault::WrongProtocol
                | Fault::SlowStart
                | Fault::AbortOnLoad
                | Fault::AbortOnDirectmlLoad
                | Fault::AbortDiarizing
                | Fault::RefuseDiarizer
                | Fault::AbortOnDiarizerLoad
                | Fault::HangOnDiarizerLoad,
            )
            | None => Ok(describe(samples, hint)),
        }
    }

    /// Needs no files; refuses, aborts or hangs as the diarizer-load faults
    /// say.
    fn load_diarizer(&mut self, _: &ModelPaths, _: usize) -> Result<(), String> {
        match self.fault {
            Some(Fault::RefuseDiarizer) => {
                Err("simulated refusal of the diarizer's models".to_owned())
            }
            Some(Fault::AbortOnDiarizerLoad) if self.fault_now().is_some() => std::process::abort(),
            Some(Fault::HangOnDiarizerLoad) if self.fault_now().is_some() => hang(),
            _ => Ok(()),
        }
    }

    fn diarize(&mut self, audio: AudioBuffer16k) -> Result<Vec<SpeakerCluster>, String> {
        match self.fault_now() {
            Some(Fault::Abort | Fault::AbortDiarizing) => std::process::abort(),
            Some(Fault::Panic) => panic!("simulated panic in the diarizer"),
            Some(Fault::Exit) => std::process::exit(3),
            Some(Fault::Hang) => hang(),
            Some(Fault::Allocate) => allocate_and_hang(),
            Some(Fault::Error) => Err("simulated failure in the diarizer".to_owned()),
            _ => Ok(describe_voices(&audio)),
        }
    }
}

/// The fake diarizer's answer: one cluster over the whole audio, with
/// confidence and embedding made from the peak so every bit of the floats has
/// to cross.
fn describe_voices(audio: &AudioBuffer16k) -> Vec<SpeakerCluster> {
    let peak = audio.samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let whole = TimeRange {
        lower: 0.0,
        upper: audio.duration(),
    };
    vec![SpeakerCluster {
        label: "Speaker 1".to_owned(),
        ranges: vec![whole],
        embedding: Some(Embedding(
            (1..=256u16).map(|i| peak * f32::from(i) / 256.0).collect(),
        )),
        cluster_confidence: peak,
        sample_clip_range: Some(whole),
    }]
}

/// The fake engine's answer: one segment naming what arrived.
fn describe(samples: &[f32], hint: Option<&LanguageTag>) -> Vec<RawSegment> {
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    // Exact up to 2^53 samples, as in `AudioBuffer16k::duration`.
    #[allow(clippy::cast_precision_loss)]
    let seconds = samples.len() as f64 / AudioBuffer16k::SAMPLE_RATE;
    vec![RawSegment {
        start: 0.0,
        end: seconds,
        text: format!("{} samples, peak {peak}", samples.len()),
        language: hint.cloned(),
        word_timings: None,
    }]
}

/// Set by [`Fault::Swell`]: [`SWOLLEN`] follows once the reply is out.
static SWELL_AFTER_REPLY: AtomicBool = AtomicBool::new(false);

/// Set after [`Fault::Swell`]'s reply: the resident set reads as `u64::MAX`.
static SWOLLEN: AtomicBool = AtomicBool::new(false);

fn hang() -> ! {
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

/// [`Fault::Allocate`]: 4 GiB in 16 MiB steps, then [`hang`].
fn allocate_and_hang() -> ! {
    let mut hoard: Vec<Vec<u8>> = Vec::new();
    while hoard.len() < 256 {
        // Touched, so the pages are resident, not just reserved.
        hoard.push(vec![1u8; 16 << 20]);
        std::thread::sleep(Duration::from_millis(5));
    }
    hang()
}

/// The resident set of this process; 0 where it cannot be read.
fn rss_bytes() -> u64 {
    if SWOLLEN.load(Ordering::Relaxed) {
        return u64::MAX;
    }
    memory_stats::memory_stats().map_or(0, |m| m.physical_mem as u64)
}

/// Writes one reply. A broken stdout means the parent is gone, so the child
/// exits quietly, on unix through `_exit`: the heartbeat thread can get here
/// while ONNX Runtime still infers on others, and `process::exit` would run the
/// atexit handlers and C++ static destructors beside them; a hang there would
/// keep the working set alive past the ignored exit signals. Nothing needs
/// flushing, stdout is gone. Any other failure is said on stderr and exits with
/// status 2.
fn send(reply: &Reply) {
    let mut out = io::stdout().lock();
    if let Err(error) = protocol::write_frame(&mut out, reply, &[]) {
        if error.kind() == io::ErrorKind::BrokenPipe {
            #[cfg(unix)]
            // SAFETY: `_exit` ends the process at once; it runs no handler
            // and touches no state the other threads hold.
            unsafe {
                libc::_exit(0);
            }
            #[cfg(not(unix))]
            std::process::exit(0);
        }
        stderr_line!("steno-speech-sidecar: could not send a reply: {error}");
        std::process::exit(2);
    }
}

/// Ignores SIGINT, SIGTERM and SIGHUP for the rest of the child's life. Called
/// first in [`serve`], so a parent that has the ready message knows they are
/// ignored; an orphaned child still ends when a heartbeat finds stdout broken.
#[cfg(unix)]
fn ignore_exit_signals() {
    for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: `SIG_IGN` installs no handler, so no code runs in a
        // signal's context.
        unsafe {
            libc::signal(signal, libc::SIG_IGN);
        }
    }
}

/// Reports the resident set every `interval` from its own thread.
fn start_heartbeat(interval: Duration) -> io::Result<()> {
    std::thread::Builder::new()
        .name("heartbeat".to_owned())
        .spawn(move || {
            let mut warned = false;
            loop {
                let rss_bytes = rss_bytes();
                if rss_bytes == 0 && !warned {
                    stderr_line!(
                        "steno-speech-sidecar: the resident set cannot be read here, so the parent's memory ceiling cannot act"
                    );
                    warned = true;
                }
                send(&Reply::Memory { rss_bytes });
                std::thread::sleep(interval);
            }
        })
        .map(drop)
}

/// Writes to stderr, once per reason, why the encoder is on the CPU though the
/// load asked for `DirectML`; the parent logs a [`FALLBACK_NOTICE`] line at
/// info.
fn tell_fallback(engine: &dyn Engine, told: &mut Option<&'static str>) {
    if let Some(reason) = engine.fallback()
        && *told != Some(reason)
    {
        stderr_line!("{FALLBACK_NOTICE} ({reason}); the speech encoder runs on the CPU");
        *told = Some(reason);
    }
}

/// Reads the samples of a transcription request and answers it. `Err` is the
/// exit code when the audio is unreadable or the engine lost its encoder; that
/// exit sends no reply, so the parent switches `DirectML` off and starts a
/// child on the CPU.
fn transcribe(
    engine: &mut dyn Engine,
    input: &mut impl io::Read,
    id: u64,
    sample_count: u64,
    hint: Option<&LanguageTag>,
) -> Result<Reply, ExitCode> {
    let samples = protocol::read_samples(input, sample_count)
        .map_err(|error| give_up("unreadable audio", error))?;
    match engine.transcribe(&samples, hint) {
        Ok(segments) => Ok(Reply::Transcript {
            id,
            segments,
            provider: engine.provider(),
        }),
        Err(_) if !engine.usable() => {
            stderr_line!(
                "steno-speech-sidecar: the speech encoder lost its session after a failed run on DirectML, and the CPU could not reopen it"
            );
            Err(ExitCode::from(70))
        }
        Err(error) => Ok(Reply::Failed { id, error }),
    }
}

/// Reads the samples of a diarization request and answers it; `Err` is the exit
/// code when the audio is unreadable.
fn diarize(
    engine: &mut dyn Engine,
    input: &mut impl io::Read,
    id: u64,
    sample_count: u64,
) -> Result<Reply, ExitCode> {
    let samples = protocol::read_samples(input, sample_count)
        .map_err(|error| give_up("unreadable audio", error))?;
    Ok(match engine.diarize(AudioBuffer16k::new(samples)) {
        Ok(clusters) => Reply::Diarization {
            id,
            clusters: clusters.iter().map(DiarizedCluster::from).collect(),
        },
        Err(error) => Reply::Failed { id, error },
    })
}

/// Says on stderr why the child stops and returns exit status 2.
fn give_up(why: &str, error: impl std::fmt::Display) -> ExitCode {
    stderr_line!("steno-speech-sidecar: {why}: {error}");
    ExitCode::from(2)
}

/// The reply to `request`; `Err` is the exit code when the child exits instead.
fn answer(
    engine: &mut dyn Engine,
    input: &mut impl io::Read,
    request: Request,
) -> Result<Reply, ExitCode> {
    Ok(match request {
        Request::Load {
            id,
            models_root,
            intra_threads,
            inter_threads,
            directml,
        } => {
            let options = OnnxOptions {
                intra_threads,
                inter_threads,
                directml,
            };
            match engine.load(&models_root, &options) {
                Ok(provider) => Reply::Loaded { id, provider },
                Err(error) => Reply::Failed { id, error },
            }
        }
        Request::Health { id } => Reply::Health {
            id,
            pid: std::process::id(),
            rss_bytes: rss_bytes(),
            loaded: engine.loaded(),
            provider: engine.provider(),
        },
        Request::Transcribe {
            id,
            sample_count,
            hint,
        } => transcribe(engine, input, id, sample_count, hint.as_ref())?,
        Request::LoadDiarizer {
            id,
            segmentation,
            embedding,
            intra_threads,
        } => {
            let models = ModelPaths {
                segmentation,
                embedding,
            };
            match engine.load_diarizer(&models, intra_threads) {
                Ok(()) => Reply::DiarizerLoaded { id },
                Err(error) => Reply::Failed { id, error },
            }
        }
        Request::Diarize { id, sample_count } => diarize(engine, input, id, sample_count)?,
        Request::Shutdown { id } => {
            send(&Reply::Bye { id });
            return Err(ExitCode::SUCCESS);
        }
    })
}

/// Runs the child until it exits, with status 0 or 2.
pub fn serve(options: &Options) -> ExitCode {
    #[cfg(unix)]
    ignore_exit_signals();
    let fake = options.fake_engine.then(|| FakeEngine {
        loaded: false,
        fault: options.fault,
        fault_once: options.fault_once.clone(),
        fell_back: false,
        lost_encoder: false,
    });
    let start_fault = fake
        .as_ref()
        .filter(|engine| engine.fault.is_some_and(Fault::at_start))
        .and_then(FakeEngine::fault_now);
    if start_fault == Some(Fault::Silent) {
        hang();
    }
    let mut engine: Box<dyn Engine> = match fake {
        Some(fake) => Box::new(fake),
        None => Box::new(OnnxEngine::default()),
    };
    if start_fault == Some(Fault::SlowStart) {
        std::thread::sleep(Duration::from_millis(200));
    }
    // Before the heartbeat starts, so `Ready` is always the first frame.
    send(&Reply::Ready {
        protocol: if start_fault == Some(Fault::WrongProtocol) {
            0
        } else {
            PROTOCOL_VERSION
        },
        pid: std::process::id(),
    });
    if let Err(error) = start_heartbeat(options.heartbeat) {
        return give_up("no heartbeat thread", error);
    }
    let mut input = BufReader::new(io::stdin().lock());
    let mut fallback_told = None;
    loop {
        let request = match protocol::read_header::<_, Request>(&mut input) {
            Ok(Some(request)) => request,
            Ok(None) => return ExitCode::SUCCESS,
            Err(error) => return give_up("unreadable request", error),
        };
        let reply = match answer(&mut *engine, &mut input, request) {
            Ok(reply) => reply,
            Err(code) => return code,
        };
        tell_fallback(&*engine, &mut fallback_told);
        send(&reply);
        if SWELL_AFTER_REPLY.load(Ordering::Relaxed) {
            SWOLLEN.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options, String> {
        Options::parse(args.iter().map(std::ffi::OsString::from))
    }

    #[test]
    fn faults_need_the_fake_engine_and_bad_arguments_are_refused() {
        let options = parse(&["--fake-engine", "--fault", "hang", "--heartbeat-ms", "20"]).unwrap();
        assert_eq!(options.fault, Some(Fault::Hang));
        assert_eq!(options.heartbeat, Duration::from_millis(20));
        assert!(
            parse(&["--fault", "abort"])
                .unwrap_err()
                .contains("--fake-engine")
        );
        assert!(parse(&["--fake-engine", "--fault", "dance"]).is_err());
        assert!(parse(&["--heartbeat-ms"]).is_err());
        assert!(parse(&["--model", "x"]).is_err());
        assert_eq!(parse(&[]).unwrap().fault, None);
    }
}
