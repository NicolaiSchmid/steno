//! The parent side: spawns `steno-speech-sidecar`, supervises it and
//! implements `SpeechEngine` over it. Every request has a deadline; the
//! reader thread checks the child's heartbeat against the memory ceiling
//! and ends the current request once it is over. A child that dies,
//! hangs, overruns the ceiling or breaks the protocol during a request is
//! killed and reaped, and the call returns [`SpeechError::Sidecar`]. One
//! that does so between requests is replaced by the next `prepare` or
//! `transcribe` without an error. Either way the next child loads the
//! models again. Nothing here can take the app down with the child.
//!
//! After a child that crashed, hung or overran the memory ceiling during a
//! load or a request with `DirectML` in use, every later child in this
//! process loads on the CPU ([`directml_switched_off`]). One that dies
//! between requests is replaced on `DirectML`: nothing ran on it since its
//! last answer; one whose end the next call does not see yet counts as
//! during that call. One that overruns the ceiling between requests still
//! counts, as what it holds then is what its last request left, on the GPU
//! too. `health`, which only the tests call, counts a death it finds like
//! one during a request. `DirectML` is asked for on Windows only.
//!
//! Swift: none; the Mac app runs `FluidAudio` in-process only.

use std::collections::{BTreeSet, VecDeque};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use steno_core::protocols::{BoundaryResult, async_trait};
use steno_core::{AudioBuffer16k, LanguageTag, RawSegment, SpeechEngine};

use super::protocol::{self, FrameError, PROTOCOL_VERSION, Reply, Request};
use crate::engine::{OnnxSpeechEngine, blocking, log_download};
use crate::error::{SidecarError, SpeechError};
use crate::model_store::{Install, ModelAsset, ModelStore};
use crate::onnx::{EncoderProvider, OnnxOptions};

/// The binary's file name, `steno-speech-sidecar` plus `.exe` on Windows.
pub const SIDECAR_BINARY: &str = if cfg!(windows) {
    "steno-speech-sidecar.exe"
} else {
    "steno-speech-sidecar"
};

/// How the client starts and limits the child.
#[derive(Debug, Clone, PartialEq)]
pub struct SidecarConfig {
    /// The `steno-speech-sidecar` binary, by absolute path: the child is
    /// handed the meeting's audio, so a bare or relative name is never
    /// looked up on `PATH` or in the working directory, and fails to start.
    pub program: PathBuf,
    /// Arguments before the ones the client adds (`--heartbeat-ms`); the
    /// app passes none, the tests choose the fake engine and a fault.
    pub args: Vec<OsString>,
    /// Session options the child opens the models with, `DirectML` for
    /// the encoder included ([`OnnxOptions::directml`]). The load asks for
    /// `DirectML` on Windows only, whatever `directml` holds elsewhere,
    /// and asks for the CPU once [`directml_switched_off`] is true.
    pub options: OnnxOptions,
    /// The child is killed once its resident set passes this many bytes.
    /// The fp32 export works in 2 to 3 GB; a 2 h recording adds about
    /// 0.5 GB of samples on each side of the pipe.
    pub memory_ceiling_bytes: u64,
    /// How often the child reports its resident set.
    pub heartbeat: Duration,
    /// From spawn to [`Reply::Ready`].
    pub startup_timeout: Duration,
    /// For [`Request::Load`]: 2.6 GB from disk and graph optimisation.
    pub load_timeout: Duration,
    /// A transcription may take this long plus
    /// [`transcribe_timeout_ratio`](Self::transcribe_timeout_ratio) times
    /// the audio's duration.
    pub transcribe_timeout_floor: Duration,
    /// Wall-clock seconds allowed per second of audio; the CPU path ran
    /// at 18 times real time on a Ryzen 7 7700 desktop (`RTFx` 18), so 1.0
    /// leaves a slow laptop a wide margin.
    pub transcribe_timeout_ratio: f64,
    /// For [`Request::Health`] and the wait for [`Reply::Bye`].
    pub control_timeout: Duration,
    /// Where the child writes a crash log when it panics
    /// (`steno_core::crash_log`, through its environment variable); the
    /// app passes its support directory, `None` writes none.
    pub crash_log_directory: Option<PathBuf>,
    /// Whether the engine downloads a missing model before it starts a
    /// child. [`Install::Never`] by default, what the app's pipelines run
    /// with: a missing file is [`SpeechError::NotInstalled`] and nothing is
    /// fetched, as Settings and onboarding install the models. The `steno`
    /// command turns it to [`Install::Allowed`] for its explicit commands.
    pub install: Install,
}

impl SidecarConfig {
    /// The defaults with `program` as the binary.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        SidecarConfig {
            program: program.into(),
            args: Vec::new(),
            options: OnnxOptions::default(),
            memory_ceiling_bytes: 6 << 30,
            heartbeat: Duration::from_millis(200),
            startup_timeout: Duration::from_secs(10),
            load_timeout: Duration::from_secs(300),
            transcribe_timeout_floor: Duration::from_secs(120),
            transcribe_timeout_ratio: 1.0,
            control_timeout: Duration::from_secs(5),
            crash_log_directory: None,
            install: Install::Never,
        }
    }

    /// The binary beside the running executable, where the installers are
    /// to put it (Tauri's `externalBin`, WP9 of
    /// `.plans/2026-10-02-rust-core-and-tauri-shell.md`).
    pub fn beside_current_exe() -> std::io::Result<Self> {
        let exe = std::env::current_exe()?;
        Ok(Self::new(exe.with_file_name(SIDECAR_BINARY)))
    }

    /// The deadline of a transcription of `seconds` of audio.
    #[must_use]
    pub fn transcribe_timeout(&self, seconds: f64) -> Duration {
        self.transcribe_timeout_floor
            + Duration::from_secs_f64((seconds * self.transcribe_timeout_ratio).max(0.0))
    }
}

/// What [`SidecarSpeechEngine::health`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidecarHealth {
    /// The child's process id.
    pub pid: u32,
    /// Its resident set, 0 where it cannot be read.
    pub rss_bytes: u64,
    /// Whether its models are loaded.
    pub loaded: bool,
    /// Where its encoder runs: the answer to its load, kept up to date
    /// from its later answers (a run that fails on `DirectML` moves the
    /// encoder to the CPU); `None` before it loaded.
    pub provider: Option<EncoderProvider>,
}

/// Backs [`directml_switched_off`]. Process-wide, so it holds for the rest
/// of the app's run and for every engine in it: the one `steno-services`
/// keeps across pipeline reloads and any other a caller builds (the CLI's,
/// the tests'), none of which asks for `DirectML` again. Nothing clears it.
static DIRECTML_SWITCHED_OFF: AtomicBool = AtomicBool::new(false);

/// Whether `DirectML` is off for the rest of the app's run: a child in
/// this process ended with `DirectML` in use in a way the module docs count
/// against it. Every engine's later loads then ask for the CPU.
#[must_use]
pub fn directml_switched_off() -> bool {
    DIRECTML_SWITCHED_OFF.load(Ordering::SeqCst)
}

/// The start of the child's stderr line that says why its encoder is on
/// the CPU though the load asked for `DirectML`, in fixed words; the
/// parent logs such a line at info level, every other line at debug.
pub const FALLBACK_NOTICE: &str = "steno-speech-sidecar: DirectML is not usable";

/// What the reader threads hand the waiting request. Heartbeats are not
/// queued, so an idle child cannot grow the queue: the reader keeps the
/// latest resident set and queues [`Event::OverCeiling`] once it is over.
enum Event {
    Reply(Reply),
    /// A heartbeat over the memory ceiling.
    OverCeiling(u64),
    /// stdout ended: between frames or inside one, which is how a child
    /// that dies while writing looks.
    Closed,
    /// stdout carried bytes that are not a frame.
    Garbage(String),
    /// The write of a request failed.
    WriteFailed(std::io::Error),
}

/// One running child.
struct SidecarProcess {
    child: Child,
    stdin: Arc<Mutex<ChildStdin>>,
    events: Receiver<Event>,
    sender: Sender<Event>,
    stderr: StderrTail,
    pid: u32,
    next_id: u64,
    /// Where the child's encoder runs; `Some` once its models are loaded.
    provider: Option<EncoderProvider>,
    /// Whether the last load asked for `DirectML`; until it is answered, a
    /// crash counts as the probe's.
    load_asked_directml: bool,
    ceiling: u64,
    /// Set by the stdout reader once it has queued a fault:
    /// [`Event::OverCeiling`], [`Event::Closed`] or [`Event::Garbage`].
    fault_queued: Arc<AtomicBool>,
}

/// The lines of the child's stderr kept for a crash report.
const STDERR_LINES: usize = 20;

/// The longest stderr line kept whole; a longer one is kept in pieces of
/// this size, so a child that writes without newlines cannot grow the
/// buffer without bound.
const STDERR_LINE_BYTES: u64 = 4096;

/// The command that starts the child with piped stdio.
fn command(config: &SidecarConfig) -> Command {
    let mut command = Command::new(&config.program);
    command
        .args(&config.args)
        .arg("--heartbeat-ms")
        .arg(config.heartbeat.as_millis().max(1).to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(directory) = &config.crash_log_directory {
        command.env(steno_core::crash_log::DIRECTORY_VARIABLE, directory);
    }
    // The child is a console program: started from the windowed app
    // without this flag, Windows opens a console window for it on every
    // job, and closing that window kills the child mid-request.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// The level a line of the child's stderr is logged at: a
/// [`FALLBACK_NOTICE`] at info, any other line at debug, as it may hold a
/// path.
fn stderr_level(line: &str) -> tracing::Level {
    if line.starts_with(FALLBACK_NOTICE) {
        tracing::Level::INFO
    } else {
        tracing::Level::DEBUG
    }
}

/// How long a crash report waits for the stderr reader to reach the end
/// of a dead child's stderr, which a loaded machine may take more than a
/// second to schedule. The wait ends as soon as stderr closes, so only a
/// child whose stderr outlives it (a grandchild holding it) costs this.
const STDERR_DRAIN: Duration = Duration::from_secs(5);

/// The last lines of the child's stderr and the thread that reads them.
struct StderrTail {
    lines: Arc<Mutex<VecDeque<String>>>,
    reader: JoinHandle<()>,
}

impl StderrTail {
    /// The lines kept, once the reader has reached the end of stderr or
    /// [`STDERR_DRAIN`] has passed, joined with newlines.
    fn after_exit(&self) -> String {
        let deadline = Instant::now() + STDERR_DRAIN;
        while !self.reader.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        lines.make_contiguous().join("\n")
    }
}

/// Reads the child's stderr on a thread of its own, logging each line at
/// its [`stderr_level`] and keeping the last [`STDERR_LINES`] for a crash
/// report. The thread ends at the end of stderr, which is how a crash
/// report knows it has the child's last words.
fn keep_stderr_tail(pid: u32, stderr: ChildStderr) -> std::io::Result<StderrTail> {
    let tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_LINES)));
    let lines = Arc::clone(&tail);
    let reader = std::thread::Builder::new()
        .name(format!("sidecar-{pid}-stderr"))
        .spawn(move || {
            let mut stderr = BufReader::new(stderr);
            let mut bytes = Vec::new();
            loop {
                bytes.clear();
                // Bytes, not `lines()`: invalid UTF-8 (an ONNX Runtime
                // message, a path) must not end the reader.
                match (&mut stderr)
                    .take(STDERR_LINE_BYTES)
                    .read_until(b'\n', &mut bytes)
                {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
                let line = String::from_utf8_lossy(&bytes)
                    .trim_end_matches(['\r', '\n'])
                    .to_owned();
                if stderr_level(&line) == tracing::Level::INFO {
                    tracing::info!(target: "steno_speech::sidecar", pid, "{line}");
                } else {
                    tracing::debug!(target: "steno_speech::sidecar", pid, "{line}");
                }
                let mut tail = lines.lock().unwrap_or_else(PoisonError::into_inner);
                if tail.len() == STDERR_LINES {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        })?;
    Ok(StderrTail {
        lines: tail,
        reader,
    })
}

impl SidecarProcess {
    /// Spawns the child and waits for its [`Reply::Ready`].
    fn spawn(config: &SidecarConfig) -> Result<Self, SidecarError> {
        if !config.program.is_absolute() {
            return Err(SidecarError::Spawn {
                program: config.program.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "not an absolute path; the sidecar is never looked up on PATH or in the working directory",
                ),
            });
        }
        let mut child = command(config)
            .spawn()
            .map_err(|source| SidecarError::Spawn {
                program: config.program.clone(),
                source,
            })?;
        let pid = child.id();
        // Before any wait on the child, so its pid names no other process.
        #[cfg(target_os = "linux")]
        super::scope::move_to_own_scope(pid);
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(SidecarError::Pipe(std::io::Error::other(
                "the child's pipes were not opened",
            )));
        };
        let (sender, events) = mpsc::channel();
        let replies = sender.clone();
        let ceiling = config.memory_ceiling_bytes;
        let fault_queued = Arc::new(AtomicBool::new(false));
        let queued = Arc::clone(&fault_queued);
        std::thread::Builder::new()
            .name(format!("sidecar-{pid}-stdout"))
            .spawn(move || {
                let mut stdout = BufReader::new(stdout);
                loop {
                    let event = match protocol::read_header::<_, Reply>(&mut stdout) {
                        Ok(Some(Reply::Memory { rss_bytes })) => {
                            if queued.load(Ordering::SeqCst) || rss_bytes <= ceiling {
                                continue;
                            }
                            Event::OverCeiling(rss_bytes)
                        }
                        Ok(Some(reply)) => Event::Reply(reply),
                        Ok(None) | Err(FrameError::Truncated | FrameError::Io(_)) => Event::Closed,
                        Err(error) => Event::Garbage(error.to_string()),
                    };
                    let fault = !matches!(event, Event::Reply(_));
                    let last = matches!(event, Event::Closed | Event::Garbage(_));
                    if replies.send(event).is_err() {
                        return;
                    }
                    // After the send: whoever reads the flag finds the
                    // event queued.
                    if fault {
                        queued.store(true, Ordering::SeqCst);
                    }
                    if last {
                        return;
                    }
                }
            })
            .map_err(SidecarError::Pipe)?;
        let tail = keep_stderr_tail(pid, stderr).map_err(SidecarError::Pipe)?;
        let mut process = SidecarProcess {
            child,
            stdin: Arc::new(Mutex::new(stdin)),
            events,
            sender,
            stderr: tail,
            pid,
            next_id: 1,
            provider: None,
            load_asked_directml: false,
            ceiling,
            fault_queued,
        };
        match process.wait_for(None, config.startup_timeout)? {
            Reply::Ready { protocol, .. } if protocol == PROTOCOL_VERSION => Ok(process),
            Reply::Ready { protocol, .. } => Err(SidecarError::Protocol(format!(
                "the child speaks protocol {protocol}, the parent {PROTOCOL_VERSION}"
            ))),
            other => Err(SidecarError::Protocol(format!(
                "expected ready, got {other:?}"
            ))),
        }
    }

    /// Sends `request` with `payload` and waits for its reply, which
    /// `pick` turns into the answer; a reply it hands back is a protocol
    /// violation, `expected` naming what it wanted. The write runs on a
    /// thread of its own, so a child that stops reading cannot block past
    /// the deadline.
    fn request<T>(
        &mut self,
        make: impl FnOnce(u64) -> Request,
        payload: Vec<u8>,
        timeout: Duration,
        expected: &str,
        pick: impl FnOnce(Reply) -> Result<T, Reply>,
    ) -> Result<T, SidecarError> {
        let id = self.next_id;
        self.next_id += 1;
        let request = make(id);
        let stdin = Arc::clone(&self.stdin);
        let failures = self.sender.clone();
        std::thread::Builder::new()
            .name(format!("sidecar-{}-stdin", self.pid))
            .spawn(move || {
                let mut stdin = stdin.lock().unwrap_or_else(PoisonError::into_inner);
                if let Err(error) = protocol::write_frame(&mut *stdin, &request, &payload) {
                    let _ = failures.send(Event::WriteFailed(error));
                }
            })
            .map_err(SidecarError::Pipe)?;
        match self.wait_for(Some(id), timeout)? {
            Reply::Failed { error, .. } => Err(SidecarError::Remote(error)),
            reply => pick(reply).map_err(|other| {
                SidecarError::Protocol(format!("expected {expected}, got {other:?}"))
            }),
        }
    }

    /// Waits for the reply to `id` (`None`: for [`Reply::Ready`]); a
    /// heartbeat over the ceiling, even one sent while idle, ends the wait.
    fn wait_for(&mut self, id: Option<u64>, timeout: Duration) -> Result<Reply, SidecarError> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(SidecarError::Timeout { after: timeout });
            }
            match self.events.recv_timeout(remaining) {
                Ok(Event::OverCeiling(rss_bytes)) => {
                    return Err(SidecarError::MemoryCeiling {
                        rss_bytes,
                        ceiling_bytes: self.ceiling,
                    });
                }
                Ok(Event::Reply(reply)) if reply.id() == id => return Ok(reply),
                Ok(Event::Reply(reply)) => {
                    return Err(SidecarError::Protocol(format!(
                        "unexpected {reply:?} while waiting for request {id:?}"
                    )));
                }
                Ok(Event::Closed) | Err(RecvTimeoutError::Disconnected) => {
                    return Err(self.crashed(None));
                }
                Ok(Event::WriteFailed(error)) => return Err(self.crashed(Some(error))),
                Ok(Event::Garbage(detail)) => return Err(SidecarError::Protocol(detail)),
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// The error for a child whose pipes closed or refused a write: its
    /// exit status and last stderr lines once it has one, else the pipe
    /// error.
    fn crashed(&mut self, write: Option<std::io::Error>) -> SidecarError {
        let Some(status) = self.exit_status(Duration::from_secs(2)) else {
            return SidecarError::Pipe(write.unwrap_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "the child closed its stdout but kept running",
                )
            }));
        };
        SidecarError::Crashed {
            status: status.to_string(),
            stderr: self.stderr.after_exit(),
        }
    }

    /// Why this child, idle since its last request, cannot take the next
    /// one: the reader queued the end of its stdout, garbage or a heartbeat
    /// over the ceiling (anything else queued between requests is a fault
    /// too), or it has exited (killed for memory by the system, say). The
    /// queue comes first, so an overrun the child then died after still
    /// counts as an overrun.
    fn failed_while_idle(&mut self) -> Option<SidecarError> {
        if let Ok(event) = self.events.try_recv() {
            return Some(match event {
                Event::OverCeiling(rss_bytes) => SidecarError::MemoryCeiling {
                    rss_bytes,
                    ceiling_bytes: self.ceiling,
                },
                Event::Closed => self.crashed(None),
                Event::Garbage(detail) => SidecarError::Protocol(detail),
                Event::WriteFailed(error) => self.crashed(Some(error)),
                Event::Reply(reply) => SidecarError::Protocol(format!("unasked {reply:?}")),
            });
        }
        matches!(self.child.try_wait(), Ok(Some(_))).then(|| self.crashed(None))
    }

    /// The exit status, waiting up to `grace` for one.
    fn exit_status(&mut self, grace: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + grace;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                _ => return None,
            }
        }
    }

    /// Asks the child to exit, then kills it if it has not within the
    /// grace period. Returns its exit status.
    fn shut_down(mut self, grace: Duration) -> Option<ExitStatus> {
        // Any answer but a failure will do.
        let polite = self
            .request(|id| Request::Shutdown { id }, Vec::new(), grace, "bye", Ok)
            .is_ok();
        if polite && let Some(status) = self.exit_status(grace) {
            return Some(status);
        }
        self.kill()
    }

    /// Kills and reaps the child.
    fn kill(&mut self) -> Option<ExitStatus> {
        let _ = self.child.kill();
        self.child.wait().ok()
    }

    /// Takes the provider a later answer reports, and logs a change (a run
    /// that failed on `DirectML` moved the encoder to the CPU); `None`
    /// keeps the one last heard.
    fn observe_provider(&mut self, reported: Option<EncoderProvider>) {
        if let (Some(current), Some(reported)) = (self.provider, reported)
            && current != reported
        {
            tracing::info!(
                pid = self.pid,
                provider = reported.as_str(),
                "the speech sidecar's encoder changed provider"
            );
            self.provider = Some(reported);
        }
    }
}

/// Whether a child's end counts against `DirectML`: it crashed, hung or
/// overran the memory ceiling with its encoder on `DirectML`, or inside a
/// load that asked for it, where the probe runs: a crash inside that load,
/// whatever its cause (the decoder, the joiner or Silero opening, a cold
/// disk past the load timeout). The memory ceiling counts because a child
/// that passes it on `DirectML` would pass it on every job; on the CPU the
/// fp32 export stays well under it. A child on the CPU, an error it
/// reported or a protocol violation does not count. A child that died
/// between requests is passed here with no provider, so it does not count
/// either; one over the ceiling then is passed as it is
/// ([`Shared::ensure_loaded`]).
fn ended_on_directml(
    error: &SidecarError,
    provider: Option<EncoderProvider>,
    load_asked_directml: bool,
) -> bool {
    matches!(
        error,
        SidecarError::Crashed { .. }
            | SidecarError::Timeout { .. }
            | SidecarError::MemoryCeiling { .. }
    ) && provider.map_or(load_asked_directml, |p| p == EncoderProvider::DirectMl)
}

/// The kind of a failure, in fixed words for the log: no stderr, no
/// protocol detail, no path.
fn failure_kind(error: &SidecarError) -> &'static str {
    match error {
        SidecarError::Spawn { .. } => "it could not start",
        SidecarError::Pipe(_) => "a pipe to it failed",
        SidecarError::Protocol(_) => "it broke the protocol",
        SidecarError::Crashed { .. } => "it died",
        SidecarError::Timeout { .. } => "it did not answer in time",
        SidecarError::MemoryCeiling { .. } => "it passed the memory ceiling",
        SidecarError::Remote(_) => "it reported an error",
        SidecarError::NotUtf8 { .. } => "the models root is not UTF-8",
    }
}

impl Drop for SidecarProcess {
    /// Never leaves a child behind, whatever path dropped this.
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            self.kill();
        }
    }
}

/// What the engine shares with the blocking threads its calls run on.
struct Shared {
    store: ModelStore,
    assets: Vec<ModelAsset>,
    config: SidecarConfig,
    /// The running child; a call holds the lock for its whole request.
    process: Mutex<Option<SidecarProcess>>,
    /// The running child's pid, 0 for none; readable while a request runs.
    pid: AtomicU32,
    spawns: AtomicU64,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Option<SidecarProcess>> {
        self.process.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Installs the assets in this process under
    /// [`Install::Allowed`]; a no-op once they are. Under [`Install::Never`]
    /// (the default) a missing file is [`SpeechError::NotInstalled`]
    /// instead, and nothing is downloaded.
    /// Blocking. `prepare` calls it before it takes the lock, so a
    /// download does not hold up `health`, `release` or a transcription in
    /// a running child; [`Shared::ensure_loaded`] calls it again under the
    /// lock, where it is a no-op after `prepare`.
    fn install(&self) -> Result<(), SpeechError> {
        // The `load` request carries the root as a JSON string.
        if self.store.root().to_str().is_none() {
            return Err(SidecarError::NotUtf8 {
                path: self.store.root().to_path_buf(),
            }
            .into());
        }
        for asset in &self.assets {
            match self.config.install {
                Install::Allowed => self.store.ensure(asset, &mut log_download)?,
                Install::Never => self.store.installed_directory(asset)?,
            };
        }
        Ok(())
    }

    /// Whether the next load asks for `DirectML`: on Windows, when the
    /// options ask for it and [`directml_switched_off`] is false.
    fn wants_directml(&self) -> bool {
        cfg!(windows) && self.config.options.directml && !directml_switched_off()
    }

    /// Makes sure a child runs with its models loaded, installing them
    /// first if need be. A child that failed since its last request is
    /// replaced without an error: on `DirectML` still if it died, on the
    /// CPU if it overran the memory ceiling on `DirectML` (see the module
    /// docs). A child that crashed, hung or overran the ceiling inside a
    /// load that asked for `DirectML` (the probe) switched `DirectML` off;
    /// no audio was sent yet, so a new child loads on the CPU within the
    /// same call. Blocking; the caller holds the lock.
    fn ensure_loaded(&self, slot: &mut Option<SidecarProcess>) -> Result<(), SpeechError> {
        if let Some(process) = slot.as_mut()
            && let Some(error) = process.failed_while_idle()
        {
            // A death between requests does not count against `DirectML`,
            // an overrun does (see the module docs); with no provider,
            // `kill_unless_remote` does not count it.
            if !matches!(error, SidecarError::MemoryCeiling { .. }) {
                process.provider = None;
                process.load_asked_directml = false;
            }
            self.kill_unless_remote(slot, error);
        }
        if slot.as_ref().is_some_and(|p| p.provider.is_some()) {
            return Ok(());
        }
        self.install()?;
        Ok(self.load(slot, self.wants_directml())?)
    }

    /// Has the running child, or a new one, load the models, asking for
    /// `DirectML` when `directml`. A child that fails is killed unless it
    /// reported the error itself; one the probe ended is replaced, within
    /// this call, by a child that loads on the CPU.
    fn load(&self, slot: &mut Option<SidecarProcess>, directml: bool) -> Result<(), SidecarError> {
        let process = if let Some(process) = slot.take() {
            process
        } else {
            let process = SidecarProcess::spawn(&self.config)?;
            self.spawns.fetch_add(1, Ordering::SeqCst);
            self.pid.store(process.pid, Ordering::SeqCst);
            process
        };
        let process = slot.insert(process);
        process.load_asked_directml = directml;
        let reply = process.request(
            |id| Request::Load {
                id,
                models_root: self.store.root().to_path_buf(),
                intra_threads: self.config.options.intra_threads,
                inter_threads: self.config.options.inter_threads,
                directml,
            },
            Vec::new(),
            self.config.load_timeout,
            "loaded",
            |reply| match reply {
                Reply::Loaded { provider, .. } => Ok(provider),
                other => Err(other),
            },
        );
        match reply {
            Ok(provider) => {
                tracing::info!(
                    pid = process.pid,
                    provider = provider.as_str(),
                    directml_requested = directml,
                    "speech sidecar loaded"
                );
                process.provider = Some(provider);
                Ok(())
            }
            // The child answered and still runs, so the probe did not end it.
            Err(error @ SidecarError::Remote(_)) => {
                process.load_asked_directml = false;
                Err(error)
            }
            Err(error) => {
                let probe_ended = ended_on_directml(&error, None, directml);
                let error = self.kill_unless_remote(slot, error);
                if probe_ended {
                    // `DirectML` is off now, so this load asks for the CPU
                    // and cannot come back here.
                    return self.load(slot, false);
                }
                Err(error)
            }
        }
    }

    /// Kills the child unless `error` came from the child itself, which
    /// then still runs and answers; returns the error. The warning names
    /// the kind of failure in fixed words; the error itself, whose crash
    /// report holds the child's stderr (which may hold a path), goes to
    /// debug only.
    fn kill_unless_remote(
        &self,
        slot: &mut Option<SidecarProcess>,
        error: SidecarError,
    ) -> SidecarError {
        if !matches!(error, SidecarError::Remote(_)) {
            if let Some(mut process) = slot.take() {
                let status = process.kill();
                tracing::warn!(
                    pid = process.pid,
                    ?status,
                    reason = failure_kind(&error),
                    "speech sidecar killed"
                );
                tracing::debug!(pid = process.pid, %error, "the speech sidecar's error");
                if ended_on_directml(&error, process.provider, process.load_asked_directml)
                    && !DIRECTML_SWITCHED_OFF.swap(true, Ordering::SeqCst)
                {
                    tracing::info!(
                        "the speech sidecar ended while DirectML was in use; the speech encoder runs on the CPU for the rest of the app's run"
                    );
                }
            }
            self.pid.store(0, Ordering::SeqCst);
        }
        error
    }
}

/// Parakeet v3 on ONNX Runtime in `steno-speech-sidecar`, the default
/// engine on Linux and Windows and the fallback on macOS (see
/// [`SpeechRuntime`](crate::SpeechRuntime)).
///
/// `prepare` installs the models into the store in this process (the
/// child never opens a connection), spawns the child and has it load them;
/// `transcribe` sends the samples over the pipe. A failed child is
/// replaced on the next call. [`SpeechEngine::release`] stops the child
/// and frees its working set; the pipeline (`steno-pipeline`) calls it
/// once a job's lanes are transcribed and no other job needs the engine.
///
/// ```no_run
/// use steno_core::{AudioBuffer16k, SpeechEngine};
/// use steno_speech::{ModelStore, SidecarConfig, SidecarSpeechEngine};
///
/// # async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
/// let engine = SidecarSpeechEngine::new(
///     ModelStore::from_environment(),
///     SidecarConfig::beside_current_exe()?,
/// );
/// // Installs the models here, then starts the child and loads them there.
/// engine.prepare().await?;
/// let segments = engine.transcribe(&AudioBuffer16k::silence(1.0), None).await?;
/// assert!(segments.is_empty());
/// // After the job: the child exits and its 2.2 GB go back.
/// engine.release().await?;
/// # Ok(())
/// # }
/// ```
pub struct SidecarSpeechEngine {
    shared: Arc<Shared>,
    languages: BTreeSet<LanguageTag>,
}

impl SidecarSpeechEngine {
    /// An engine over `store`; nothing starts until `prepare`.
    #[must_use]
    pub fn new(store: ModelStore, config: SidecarConfig) -> Self {
        Self::with_assets(
            store,
            config,
            vec![ModelAsset::silero_vad(), ModelAsset::parakeet_v3_fp32()],
        )
    }

    /// For tests: installs `assets` instead of Silero and the export
    /// before the child starts. The tests pass none, as their fake child
    /// needs none.
    #[must_use]
    pub fn with_assets(store: ModelStore, config: SidecarConfig, assets: Vec<ModelAsset>) -> Self {
        SidecarSpeechEngine {
            shared: Arc::new(Shared {
                store,
                assets,
                config,
                process: Mutex::new(None),
                pid: AtomicU32::new(0),
                spawns: AtomicU64::new(0),
            }),
            languages: OnnxSpeechEngine::LANGUAGES
                .into_iter()
                .map(LanguageTag::from)
                .collect(),
        }
    }

    /// How the engine starts and limits the child.
    #[must_use]
    pub fn config(&self) -> &SidecarConfig {
        &self.shared.config
    }

    /// The store `prepare` installs the models into and the child loads
    /// them from.
    #[must_use]
    pub fn store(&self) -> &ModelStore {
        &self.shared.store
    }

    /// The running child's pid; `None` when there is none. Readable while
    /// a request runs.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        Some(self.shared.pid.load(Ordering::SeqCst)).filter(|&pid| pid != 0)
    }

    /// For tests: how many children this engine has started.
    #[must_use]
    pub fn spawns(&self) -> u64 {
        self.shared.spawns.load(Ordering::SeqCst)
    }

    /// For tests: whether a fault of the running child is queued for the
    /// next call to find: the end of its stdout, garbage on it or a report
    /// of a resident set over the ceiling. Blocking, and it waits for a
    /// request that runs.
    #[doc(hidden)]
    #[must_use]
    pub fn fault_queued(&self) -> bool {
        self.shared
            .lock()
            .as_ref()
            .is_some_and(|p| p.fault_queued.load(Ordering::SeqCst))
    }

    /// Asks the running child for its pid, resident set, whether its
    /// models are loaded and where its encoder runs; `None` when no child
    /// runs. A child that does not answer is killed. Unlike `prepare` and
    /// `transcribe`, it does not look for a child that failed since its
    /// last request first, so a child that died on `DirectML` meanwhile
    /// counts against `DirectML` here as during a request; only the tests
    /// call it.
    pub async fn health(&self) -> BoundaryResult<Option<SidecarHealth>> {
        let shared = Arc::clone(&self.shared);
        Ok(blocking(move || {
            let mut slot = shared.lock();
            let Some(process) = slot.as_mut() else {
                return Ok(None);
            };
            let reply = process.request(
                |id| Request::Health { id },
                Vec::new(),
                shared.config.control_timeout,
                "health",
                |reply| match reply {
                    Reply::Health {
                        pid,
                        rss_bytes,
                        loaded,
                        provider,
                        ..
                    } => Ok(SidecarHealth {
                        pid,
                        rss_bytes,
                        loaded,
                        provider,
                    }),
                    other => Err(other),
                },
            );
            match reply {
                Ok(mut health) => {
                    process.observe_provider(health.provider);
                    health.provider = process.provider;
                    Ok(Some(health))
                }
                Err(error) => Err(shared.kill_unless_remote(&mut slot, error)),
            }
        })
        .await??)
    }

    /// [`SpeechEngine::release`] with the child's exit status: stops the
    /// child, politely first; `None` when no child ran. The next call
    /// starts a new one.
    pub async fn shut_down(&self) -> BoundaryResult<Option<ExitStatus>> {
        let shared = Arc::clone(&self.shared);
        Ok(blocking(move || {
            let mut slot = shared.lock();
            shared.pid.store(0, Ordering::SeqCst);
            slot.take()
                .and_then(|process| process.shut_down(shared.config.control_timeout))
        })
        .await?)
    }
}

impl Drop for SidecarSpeechEngine {
    /// Stops the child politely when nothing else holds the state; the
    /// process's own drop kills it otherwise. The polite stop waits up to
    /// twice the control timeout, so inside a tokio runtime it runs on a
    /// blocking thread instead of the worker that dropped the engine.
    fn drop(&mut self) {
        if let Some(shared) = Arc::get_mut(&mut self.shared)
            && let Some(process) = shared
                .process
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
        {
            shared.pid.store(0, Ordering::SeqCst);
            let grace = shared.config.control_timeout;
            let stop = move || {
                let _ = process.shut_down(grace);
            };
            match tokio::runtime::Handle::try_current() {
                // A runtime shutting down drops the task unrun, and with it
                // the process, whose drop kills the child.
                Ok(runtime) => drop(runtime.spawn_blocking(stop)),
                Err(_) => stop(),
            }
        }
    }
}

#[async_trait]
impl SpeechEngine for SidecarSpeechEngine {
    fn id(&self) -> &str {
        OnnxSpeechEngine::ID
    }

    fn supported_languages(&self) -> &BTreeSet<LanguageTag> {
        &self.languages
    }

    async fn prepare(&self) -> BoundaryResult<()> {
        let shared = Arc::clone(&self.shared);
        blocking(move || {
            // The download runs before the lock; `ensure_loaded` then finds
            // the files in place.
            shared.install()?;
            shared.ensure_loaded(&mut shared.lock())
        })
        .await??;
        Ok(())
    }

    async fn transcribe(
        &self,
        audio: &AudioBuffer16k,
        hint: Option<&LanguageTag>,
    ) -> BoundaryResult<Vec<RawSegment>> {
        if audio.is_empty() {
            return Ok(Vec::new());
        }
        let shared = Arc::clone(&self.shared);
        let sample_count = audio.samples.len() as u64;
        let timeout = shared.config.transcribe_timeout(audio.duration());
        let payload = protocol::encode_samples(&audio.samples);
        let hint = hint.cloned();
        let segments = blocking(move || {
            let mut slot = shared.lock();
            shared.ensure_loaded(&mut slot)?;
            let process = slot.as_mut().ok_or(SpeechError::NotPrepared)?;
            let reply = process.request(
                |id| Request::Transcribe {
                    id,
                    sample_count,
                    hint,
                },
                payload,
                timeout,
                "a transcript",
                |reply| match reply {
                    Reply::Transcript {
                        segments, provider, ..
                    } => Ok((segments, provider)),
                    other => Err(other),
                },
            );
            match reply {
                Ok((segments, provider)) => {
                    process.observe_provider(provider);
                    Ok(segments)
                }
                Err(error) => Err(SpeechError::from(
                    shared.kill_unless_remote(&mut slot, error),
                )),
            }
        })
        .await??;
        Ok(segments)
    }

    /// Stops the child and frees its working set (the models, 2.2 GB).
    async fn release(&self) -> BoundaryResult<()> {
        self.shut_down().await.map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deadline_grows_with_the_audio_and_the_binary_sits_beside_the_app() {
        let config = SidecarConfig::new("/opt/steno/steno-speech-sidecar");
        assert_eq!(config.transcribe_timeout(0.0), Duration::from_secs(120));
        assert_eq!(config.transcribe_timeout(3600.0), Duration::from_secs(3720));
        let beside = SidecarConfig::beside_current_exe().unwrap();
        assert_eq!(beside.program.file_name().unwrap(), SIDECAR_BINARY);
        assert_eq!(
            beside.program.parent(),
            std::env::current_exe().unwrap().parent()
        );
    }

    #[test]
    fn the_child_is_started_with_the_configured_heartbeat() {
        let mut config = SidecarConfig::new("steno-speech-sidecar");
        config.args = vec!["--fake-engine".into()];
        config.heartbeat = Duration::from_millis(20);
        let command = command(&config);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["--fake-engine", "--heartbeat-ms", "20"]);
    }

    #[tokio::test]
    async fn a_missing_binary_is_an_error_not_a_panic_and_needs_no_child_for_empty_audio() {
        let dir = tempfile::tempdir().unwrap();
        let engine = SidecarSpeechEngine::with_assets(
            ModelStore::new(dir.path()),
            SidecarConfig::new(dir.path().join("no-such-sidecar")),
            Vec::new(),
        );
        assert_eq!(engine.id(), "parakeet-v3");
        assert_eq!(engine.supported_languages().len(), 25);
        assert!(
            engine
                .transcribe(&AudioBuffer16k::default(), None)
                .await
                .unwrap()
                .is_empty()
        );
        let error = engine.prepare().await.unwrap_err().to_string();
        assert!(error.contains("could not start"), "{error}");
        assert_eq!(engine.pid(), None);
        assert_eq!(engine.spawns(), 0);
        assert_eq!(engine.health().await.unwrap(), None);
        assert_eq!(engine.shut_down().await.unwrap(), None);
        engine.release().await.unwrap();
    }

    /// A bare or relative name is refused, never looked up: `sh` (`cmd`
    /// on Windows) is on every `PATH`, and still nothing starts; nor does
    /// `./sh` (`.\cmd`) from the working directory.
    #[tokio::test]
    async fn a_program_that_is_not_an_absolute_path_never_starts() {
        let dir = tempfile::tempdir().unwrap();
        let names = if cfg!(windows) {
            ["cmd", ".\\cmd"]
        } else {
            ["sh", "./sh"]
        };
        for program in names {
            let engine = SidecarSpeechEngine::with_assets(
                ModelStore::new(dir.path()),
                SidecarConfig::new(program),
                Vec::new(),
            );
            let error = engine.prepare().await.unwrap_err().to_string();
            assert!(error.contains("could not start"), "{program}: {error}");
            assert!(error.contains("not an absolute path"), "{program}: {error}");
            assert_eq!(engine.spawns(), 0, "{program}");
            assert_eq!(engine.pid(), None, "{program}");
        }
    }

    #[test]
    fn only_a_crash_a_hang_or_an_overrun_with_directml_in_use_turns_it_off() {
        use EncoderProvider::{Cpu, DirectMl};
        let crashed = SidecarError::Crashed {
            status: "signal: 6".to_owned(),
            stderr: String::new(),
        };
        let hung = SidecarError::Timeout {
            after: Duration::from_secs(1),
        };
        let over = SidecarError::MemoryCeiling {
            rss_bytes: 2,
            ceiling_bytes: 1,
        };
        for error in [&crashed, &hung, &over] {
            assert!(ended_on_directml(error, Some(DirectMl), false));
            // Inside the load that asked for it: the probe.
            assert!(ended_on_directml(error, None, true));
            // On the CPU, the probe's fallback included.
            assert!(!ended_on_directml(error, Some(Cpu), true));
            assert!(!ended_on_directml(error, None, false));
        }
        for error in [
            SidecarError::Remote("no".to_owned()),
            SidecarError::Protocol("garbage".to_owned()),
        ] {
            assert!(!ended_on_directml(&error, Some(DirectMl), true));
        }
    }

    #[test]
    fn only_the_fallback_notice_reaches_info_and_a_path_stays_at_debug() {
        use tracing::Level;
        let notice = format!(
            "{FALLBACK_NOTICE} (a run on DirectML failed); the speech encoder runs on the CPU"
        );
        assert_eq!(stderr_level(&notice), Level::INFO);
        for line in [
            "C:\\Users\\someone\\AppData\\Local\\Steno\\models\\encoder.onnx: not found",
            "/home/someone/.local/share/steno/models/encoder.onnx: not found",
            "steno-speech-sidecar: unreadable audio: truncated",
            "",
        ] {
            assert_eq!(stderr_level(line), Level::DEBUG, "{line}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_models_root_the_protocol_cannot_carry_is_refused_before_any_spawn() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(std::ffi::OsStr::from_bytes(b"models-\xff"));
        let engine = SidecarSpeechEngine::with_assets(
            ModelStore::new(&root),
            SidecarConfig::new(dir.path().join("no-such-sidecar")),
            Vec::new(),
        );
        let error = engine.prepare().await.unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<SpeechError>(),
                Some(SpeechError::Sidecar(SidecarError::NotUtf8 { path })) if *path == root
            ),
            "{error}"
        );
        assert_eq!(engine.spawns(), 0);
    }

    /// By default (`Install::Never`, the app's pipelines), a missing model
    /// is `NotInstalled` from `prepare` and from `transcribe`, which
    /// restarts a child: no request reaches the host and no child starts.
    /// Under `Install::Allowed` (the `steno` command) the engine asks the
    /// host for it.
    #[tokio::test]
    async fn by_default_a_missing_model_is_refused_and_nothing_is_fetched() {
        use crate::model_store::{ModelFile, ModelSource};
        let dir = tempfile::tempdir().unwrap();
        // A host that answers every request 404 and counts them.
        let host = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = host.local_addr().unwrap();
        let requests = Arc::new(AtomicU64::new(0));
        std::thread::spawn({
            let requests = Arc::clone(&requests);
            move || {
                for stream in host.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    requests.fetch_add(1, Ordering::SeqCst);
                    let mut head = [0u8; 1024];
                    let _ = std::io::Read::read(&mut stream, &mut head);
                    let _ = std::io::Write::write_all(
                        &mut stream,
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
            }
        });
        let asset = ModelAsset {
            id: "test-asset".to_owned(),
            display_name: "Test".to_owned(),
            licence: "MIT".to_owned(),
            attribution: String::new(),
            files: vec![ModelFile {
                name: "model.onnx".to_owned(),
                source: Some(ModelSource::Url(format!("http://{address}/model.onnx"))),
                sha256: "0".repeat(64),
                size: 4,
            }],
        };
        let engine = |install: Option<Install>| {
            let mut config = SidecarConfig::new(dir.path().join("no-such-sidecar"));
            if let Some(install) = install {
                config.install = install;
            }
            config.control_timeout = Duration::from_millis(200);
            SidecarSpeechEngine::with_assets(
                ModelStore::new(dir.path()),
                config,
                vec![asset.clone()],
            )
        };
        let refusing = engine(None);
        let not_installed = |error: steno_core::protocols::BoxError| {
            assert!(
                matches!(
                    error.downcast_ref::<SpeechError>(),
                    Some(SpeechError::NotInstalled { missing, .. }) if missing == &["model.onnx"]
                ),
                "{error}"
            );
        };
        not_installed(refusing.prepare().await.unwrap_err());
        not_installed(
            refusing
                .transcribe(&AudioBuffer16k::silence(1.0), None)
                .await
                .unwrap_err(),
        );
        assert_eq!(refusing.spawns(), 0);
        assert_eq!(
            requests.load(Ordering::SeqCst),
            0,
            "no download was asked for"
        );
        let error = engine(Some(Install::Allowed))
            .prepare()
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("404"), "{error}");
        assert_eq!(
            requests.load(Ordering::SeqCst),
            1,
            "`Install::Allowed` asks the host"
        );
    }
}
