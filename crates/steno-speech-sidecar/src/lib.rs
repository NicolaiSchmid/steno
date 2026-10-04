//! `steno-speech-sidecar`, the speech sidecar: the child process that runs
//! Parakeet on ONNX Runtime for the app (decision 5 of
//! `.plans/2026-10-01-cross-platform-speech-stack.md`, invariant 4 of
//! `.plans/2026-10-02-rust-core-and-tauri-shell.md`). The app, its parent,
//! drives it through `steno_speech::SidecarSpeechEngine`; the wire format
//! is `steno_speech::sidecar::protocol`.
//!
//! - [`serve`]: the child's whole life, from the ready message to exit.
//! - [`Options::parse`]: the command line the parent passes.
//! - [`Fault`]: what the fake engine does wrong for the isolation tests.
//!
//! Swift: none; the Mac app runs `FluidAudio` in-process only.
//!
//! # Platform policy
//!
//! On Linux and Windows this process is where speech runs, always: the app
//! never runs Parakeet on ONNX Runtime itself (the diarizer's ONNX models
//! still run in the app's process). On macOS the in-process `CoreML` engine
//! is the default and this sidecar is a fallback behind the speech setting
//! `onnxSidecarOnMac` (`steno_speech::SpeechSettings`). The reason is the
//! same everywhere: an uncaught C++ exception in ONNX Runtime ends the
//! process, and ONNX Runtime works in 2 to 3 GB; in a child such an end
//! costs one request, and the memory goes back when the child exits.
//!
//! # What the child does
//!
//! It loads the models once from the store root the parent names and
//! installed; it never downloads and opens no connection. When the parent
//! asks for `DirectML` on Windows, the encoder runs there if the probe in
//! `steno_speech::onnx` passes, and the child answers the load with the
//! provider it chose. Its health reports and transcripts carry the provider
//! in force, and when the encoder falls back to the CPU it writes one line
//! saying why, in fixed words. It reads framed requests from stdin with the
//! audio as a binary payload, answers on stdout, and reports its resident
//! set from a heartbeat thread so the parent can kill it at the memory
//! ceiling. It exits on a shutdown request and as soon as stdin ends or
//! stdout breaks, so a dead parent leaves no child behind. Its log goes to
//! stderr, which the parent logs and keeps the tail of for crash reports.
//!
//! # Privacy
//!
//! Its sessions open through `steno_speech::onnx`, which switches ONNX
//! Runtime's telemetry off first, so ONNX Runtime sends nothing. With
//! `DirectML` on, `DirectML.dll` and Direct3D 12 may still log to Windows'
//! own diagnostic data, as for any program that uses them; the child opens
//! nothing for it, and no audio or text is involved.
//!
//! # Test faults
//!
//! `--fake-engine` replaces Parakeet with an engine that needs no models
//! and answers with the sample count and peak of the audio it received; it
//! answers a load that asks for `DirectML` with `DirectML`, and reports no
//! live provider until `--fault fallback`. Only with it, `--fault <kind>`
//! ([`Fault`]) makes the next transcription abort, panic, flood stderr and
//! panic, exit, hang, allocate 4 GiB, write garbage, fail or report a
//! fallback to the CPU, the child abort on a load that asks for
//! `DirectML`, or stay silent or announce another protocol version from
//! the start;
//! `--fault-once <path>` limits that to the first child that creates
//! `<path>`, which holds that child's pid. The isolation tests drive the
//! real client against these.

use std::fs::File;
use std::io::{self, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use steno_core::{AudioBuffer16k, LanguageTag, RawSegment};
use steno_speech::sidecar::FALLBACK_NOTICE;
use steno_speech::sidecar::protocol::{self, PROTOCOL_VERSION, Reply, Request};
use steno_speech::{
    EncoderProvider, ModelStore, OnnxBackend, OnnxOptions, OnnxSpeechEngine, PipelineConfig,
    Transcriber, VadConfig,
};

steno_core::string_enum! {
    /// What the next transcription of the fake engine does instead of
    /// answering.
    pub enum Fault {
        /// `std::process::abort`, the way an uncaught C++ exception in ONNX
        /// Runtime ends the process.
        Abort = "abort",
        /// A line of stderr that is not UTF-8, then a Rust panic, which
        /// ends the process with status 101.
        Panic = "panic",
        /// 1 MiB of stderr without a newline, then a Rust panic: the
        /// parent's crash report must stay small.
        Flood = "flood",
        /// Exits with status 3.
        Exit = "exit",
        /// Never answers.
        Hang = "hang",
        /// Allocates and touches 4 GiB in 16 MiB steps, then hangs.
        Allocate = "allocate",
        /// Writes bytes that are not a frame (a length prefix of 16 MiB,
        /// then text), then hangs.
        Garbage = "garbage",
        /// Answers with an error and keeps running.
        Error = "error",
        /// Answers, and from then on reports the CPU as the encoder's
        /// provider and writes why to stderr, as a child does after a run
        /// that failed on `DirectML`.
        Fallback = "fallback",
        /// `std::process::abort` inside a load that asks for `DirectML`,
        /// the way a driver may end the probe; a load on the CPU works.
        AbortOnDirectmlLoad = "abort-on-directml-load",
        /// At start: sends nothing, reads nothing, hangs.
        Silent = "silent",
        /// At start: announces protocol version 0 in its ready message.
        WrongProtocol = "wrong-protocol",
    }
}

impl Fault {
    /// Committed when the child starts, not at the next transcription.
    fn at_start(self) -> bool {
        matches!(self, Fault::Silent | Fault::WrongProtocol)
    }

    /// Committed at a load, not at the next transcription.
    fn at_load(self) -> bool {
        self == Fault::AbortOnDirectmlLoad
    }
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
    /// Parses `--fake-engine`, `--fault <kind>`, `--fault-once <path>` and
    /// `--heartbeat-ms <n>`; faults need the fake engine.
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
    /// Where the encoder runs now; `None` before a load, or when the
    /// engine cannot tell.
    fn provider(&self) -> Option<EncoderProvider>;
    /// Why the encoder is on the CPU though the load asked for `DirectML`,
    /// in fixed words ([`OnnxBackend::fallback`]).
    fn fallback(&self) -> Option<&'static str> {
        None
    }
    fn transcribe(
        &mut self,
        samples: &[f32],
        hint: Option<&LanguageTag>,
    ) -> Result<Vec<RawSegment>, String>;
}

/// Parakeet on ONNX Runtime from an installed store.
#[derive(Default)]
struct OnnxEngine {
    transcriber: Option<Transcriber<OnnxBackend>>,
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
                    PipelineConfig::default(),
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
}

/// No models: one segment naming what arrived, or the configured fault.
struct FakeEngine {
    loaded: bool,
    fault: Option<Fault>,
    fault_once: Option<PathBuf>,
    /// Set by [`Fault::Fallback`].
    fell_back: bool,
}

impl FakeEngine {
    /// The fault to commit now: always, or with `--fault-once` only in
    /// the child that creates the marker, which then holds its pid.
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
    /// Reports `DirectML` when asked for it, as if the probe had passed, so
    /// the tests see the setting reach the child and the answer come back;
    /// with [`Fault::AbortOnDirectmlLoad`], aborts there instead.
    fn load(&mut self, _: &Path, options: &OnnxOptions) -> Result<EncoderProvider, String> {
        if options.directml && self.fault.is_some_and(Fault::at_load) && self.fault_now().is_some()
        {
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

    /// `None`, as from a child that does not say, until
    /// [`Fault::Fallback`].
    fn provider(&self) -> Option<EncoderProvider> {
        self.fell_back.then_some(EncoderProvider::Cpu)
    }

    /// Fixed words of its own after [`Fault::Fallback`].
    fn fallback(&self) -> Option<&'static str> {
        self.fell_back.then_some("the fake engine fell back")
    }

    fn transcribe(
        &mut self,
        samples: &[f32],
        hint: Option<&LanguageTag>,
    ) -> Result<Vec<RawSegment>, String> {
        match self.fault_now() {
            Some(Fault::Abort) => std::process::abort(),
            Some(Fault::Panic) => {
                // A line that is not UTF-8 first, as a native library may
                // write: the parent must still keep the panic message.
                let _ = io::stderr().write_all(b"native noise \xff\xfe\n");
                panic!("simulated panic in the speech engine")
            }
            Some(Fault::Flood) => {
                let _ = io::stderr().write_all(&vec![b'x'; 1 << 20]);
                panic!("simulated panic after a flood of stderr")
            }
            Some(Fault::Exit) => std::process::exit(3),
            Some(Fault::Hang) => hang(),
            Some(Fault::Allocate) => {
                let mut hoard: Vec<Vec<u8>> = Vec::new();
                while hoard.len() < 256 {
                    // Touched, so the pages are resident, not just reserved.
                    hoard.push(vec![1u8; 16 << 20]);
                    std::thread::sleep(Duration::from_millis(5));
                }
                hang()
            }
            Some(Fault::Garbage) => {
                let mut out = io::stdout().lock();
                // Under the header limit, so only the first byte after
                // it can tell the parent this is no frame.
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
            // The start and load faults were committed, if at all, there.
            Some(Fault::Silent | Fault::WrongProtocol | Fault::AbortOnDirectmlLoad) | None => {
                Ok(describe(samples, hint))
            }
        }
    }
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

fn hang() -> ! {
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

/// The resident set of this process; 0 where it cannot be read.
fn rss_bytes() -> u64 {
    memory_stats::memory_stats().map_or(0, |m| m.physical_mem as u64)
}

/// Writes one reply; a broken stdout means the parent is gone, so the
/// child exits.
fn send(reply: &Reply) {
    let mut out = io::stdout().lock();
    if protocol::write_frame(&mut out, reply, &[]).is_err() {
        std::process::exit(0);
    }
}

/// Reports the resident set every `interval` from a thread of its own,
/// between and during requests.
fn start_heartbeat(interval: Duration) -> io::Result<()> {
    std::thread::Builder::new()
        .name("heartbeat".to_owned())
        .spawn(move || {
            let mut warned = false;
            loop {
                let rss_bytes = rss_bytes();
                if rss_bytes == 0 && !warned {
                    eprintln!(
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

/// Writes to stderr why the encoder is on the CPU though the load asked
/// for `DirectML`, once per reason: the child has no log subscriber, and
/// the parent logs its stderr, a line that starts with
/// [`FALLBACK_NOTICE`] at info level.
fn tell_fallback(engine: &dyn Engine, told: &mut Option<&'static str>) {
    if let Some(reason) = engine.fallback()
        && *told != Some(reason)
    {
        eprintln!("{FALLBACK_NOTICE} ({reason}); the speech encoder runs on the CPU");
        *told = Some(reason);
    }
}

/// Runs the child until a shutdown request, the end of stdin, a broken
/// stdout or an unreadable request.
pub fn serve(options: &Options) -> ExitCode {
    let fake = options.fake_engine.then(|| FakeEngine {
        loaded: false,
        fault: options.fault,
        fault_once: options.fault_once.clone(),
        fell_back: false,
    });
    let start_fault = fake
        .as_ref()
        .filter(|engine| engine.fault.is_some_and(Fault::at_start))
        .and_then(FakeEngine::fault_now);
    if start_fault == Some(Fault::Silent) {
        hang();
    }
    if let Err(error) = start_heartbeat(options.heartbeat) {
        eprintln!("steno-speech-sidecar: no heartbeat thread: {error}");
        return ExitCode::from(2);
    }
    let mut engine: Box<dyn Engine> = match fake {
        Some(fake) => Box::new(fake),
        None => Box::new(OnnxEngine::default()),
    };
    send(&Reply::Ready {
        protocol: if start_fault == Some(Fault::WrongProtocol) {
            0
        } else {
            PROTOCOL_VERSION
        },
        pid: std::process::id(),
    });
    let mut input = BufReader::new(io::stdin().lock());
    let mut fallback_told = None;
    loop {
        let request = match protocol::read_header::<_, Request>(&mut input) {
            Ok(Some(request)) => request,
            Ok(None) => return ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("steno-speech-sidecar: unreadable request: {error}");
                return ExitCode::from(2);
            }
        };
        let reply = match request {
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
            } => {
                let samples = match protocol::read_samples(&mut input, sample_count) {
                    Ok(samples) => samples,
                    Err(error) => {
                        eprintln!("steno-speech-sidecar: unreadable audio: {error}");
                        return ExitCode::from(2);
                    }
                };
                match engine.transcribe(&samples, hint.as_ref()) {
                    Ok(segments) => Reply::Transcript {
                        id,
                        segments,
                        provider: engine.provider(),
                    },
                    Err(error) => Reply::Failed { id, error },
                }
            }
            Request::Shutdown { id } => {
                send(&Reply::Bye { id });
                return ExitCode::SUCCESS;
            }
        };
        tell_fallback(&*engine, &mut fallback_told);
        send(&reply);
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

    #[test]
    fn a_fault_once_marker_lets_only_the_first_child_fault() {
        let dir = tempfile::tempdir().unwrap();
        let engine = FakeEngine {
            loaded: false,
            fault: Some(Fault::Error),
            fault_once: Some(dir.path().join("marker")),
            fell_back: false,
        };
        assert_eq!(engine.fault_now(), Some(Fault::Error));
        assert_eq!(engine.fault_now(), None);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("marker")).unwrap(),
            std::process::id().to_string()
        );
        let always = FakeEngine {
            fault_once: None,
            ..engine
        };
        assert_eq!(always.fault_now(), Some(Fault::Error));
    }
}
