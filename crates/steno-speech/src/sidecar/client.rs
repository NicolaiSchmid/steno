//! The parent side: spawns `steno-speech-sidecar`, supervises it and implements
//! `SpeechEngine` over it. A child that fails during a request is killed and
//! the call returns [`SpeechError::Sidecar`]; one that fails between requests
//! is replaced. After a `DirectML` failure every later child loads on the CPU
//! ([`directml_switched_off`]).

use std::borrow::Cow;
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
use steno_core::{AudioBuffer16k, LanguageTag, RawSegment, SpeakerCluster, SpeechEngine};

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
    /// The `steno-speech-sidecar` binary, by absolute path: the child is handed
    /// the meeting's audio, so a bare or relative name is never looked up.
    pub program: PathBuf,
    /// Arguments before the ones the client adds (the tests' fake engine and
    /// fault).
    pub args: Vec<OsString>,
    /// Session options for the models; `DirectML` only on Windows and until
    /// [`directml_switched_off`].
    pub options: OnnxOptions,
    /// The child is killed once its resident set passes this many bytes.
    pub memory_ceiling_bytes: u64,
    /// How often the child reports its resident set.
    pub heartbeat: Duration,
    /// From spawn to [`Reply::Ready`].
    pub startup_timeout: Duration,
    /// For [`Request::Load`]: 2.6 GB from disk and graph optimisation.
    pub load_timeout: Duration,
    /// A run may take this long plus `transcribe_timeout_ratio` times the
    /// audio's duration.
    pub transcribe_timeout_floor: Duration,
    /// Wall-clock seconds allowed per second of audio; the CPU runs at about 18
    /// times real time.
    pub transcribe_timeout_ratio: f64,
    /// For [`Request::Health`] and the wait for [`Reply::Bye`].
    pub control_timeout: Duration,
    /// Where the child writes a crash log when it panics; `None` writes none.
    pub crash_log_directory: Option<PathBuf>,
    /// Whether a missing model may be downloaded.
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

    /// The binary beside the running executable, where the installers put it.
    pub fn beside_current_exe() -> std::io::Result<Self> {
        let exe = std::env::current_exe()?;
        Ok(Self::new(exe.with_file_name(SIDECAR_BINARY)))
    }

    /// The deadline for `seconds` of audio.
    #[must_use]
    pub fn transcribe_timeout(&self, seconds: f64) -> Duration {
        self.transcribe_timeout_floor
            + Duration::from_secs_f64((seconds * self.transcribe_timeout_ratio).max(0.0))
    }
}

/// The diarizer's two installed model files and its sessions' threads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiarizerModels {
    /// pyannote segmentation 3.0.
    pub segmentation: PathBuf,
    /// `WeSpeaker` ResNet34-LM.
    pub embedding: PathBuf,
    /// Intra-op threads for both sessions; zero lets ONNX Runtime decide.
    pub intra_threads: usize,
}

/// What [`SidecarSpeechEngine::health`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidecarHealth {
    pub pid: u32,
    /// 0 where it cannot be read.
    pub rss_bytes: u64,
    pub loaded: bool,
    /// Where its encoder runs; `None` before it loaded.
    pub provider: Option<EncoderProvider>,
}

/// Backs [`directml_switched_off`]; process-wide, and nothing clears it.
static DIRECTML_SWITCHED_OFF: AtomicBool = AtomicBool::new(false);

/// Whether a child's end has switched `DirectML` off for the rest of the run.
#[must_use]
pub fn directml_switched_off() -> bool {
    DIRECTML_SWITCHED_OFF.load(Ordering::SeqCst)
}

/// The start of the child's stderr line saying its encoder fell back to the
/// CPU.
pub const FALLBACK_NOTICE: &str = "steno-speech-sidecar: DirectML is not usable";

/// What the reader threads hand the waiting request; heartbeats are not queued.
enum Event {
    Reply(Reply),
    OverCeiling(u64),
    Closed,
    Garbage(String),
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
    provider: Option<EncoderProvider>,
    /// Whether the last load asked for `DirectML`; a crash before its answer
    /// counts as the probe's.
    load_asked_directml: bool,
    diarizer: Option<DiarizerModels>,
    ceiling: u64,
    /// Set by the stdout reader once it has queued a fault.
    fault_queued: Arc<AtomicBool>,
}

/// The lines of the child's stderr kept for a crash report.
const STDERR_LINES: usize = 20;

/// The longest stderr line kept whole; longer ones are split.
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
    // Without this flag Windows opens a console window for the child on every
    // job, and closing it kills the child.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// The level a stderr line is logged at; any but a [`FALLBACK_NOTICE`] may hold
/// a path.
fn stderr_level(line: &str) -> tracing::Level {
    if line.starts_with(FALLBACK_NOTICE) {
        tracing::Level::INFO
    } else {
        tracing::Level::DEBUG
    }
}

/// How long a crash report waits for the stderr reader to reach the end of a
/// dead child's stderr.
const STDERR_DRAIN: Duration = Duration::from_secs(5);

/// The last lines of the child's stderr and the thread that reads them.
struct StderrTail {
    lines: Arc<Mutex<VecDeque<String>>>,
    reader: JoinHandle<()>,
}

impl StderrTail {
    /// The lines kept, once the reader reached the end of stderr or
    /// [`STDERR_DRAIN`] passed.
    fn after_exit(&self) -> String {
        let deadline = Instant::now() + STDERR_DRAIN;
        while !self.reader.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        lines.make_contiguous().join("\n")
    }
}

/// Reads the child's stderr on its own thread, logging each line and keeping
/// the last [`STDERR_LINES`].
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
                // Bytes, not `lines()`: invalid UTF-8 must not end the reader.
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
                    // After the send, so whoever reads the flag finds the event
                    // queued.
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
            diarizer: None,
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

    /// Sends `request` and waits for the reply `pick` accepts; anything else is
    /// a protocol violation. The write runs on its own thread, so a child that
    /// stops reading cannot block past the deadline.
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

    /// Waits for the reply to `id` (`None`: [`Reply::Ready`]); a heartbeat over
    /// the ceiling ends the wait.
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

    /// The error for a child whose pipes failed: its exit status and stderr
    /// once it has one, else the pipe error.
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

    /// Why this idle child cannot take the next request: a queued fault, or it
    /// has exited. The queue comes first, so an overrun still counts as one.
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

    /// Asks the child to exit, then kills it after the grace period.
    fn shut_down(mut self, grace: Duration) -> Option<ExitStatus> {
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

    /// Takes the provider a later answer reports; `None` keeps the last one.
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

/// Whether a child's end counts against `DirectML`: it crashed, hung or overran
/// the memory ceiling with its encoder on `DirectML`, or inside a load that
/// asked for it (the probe). The ceiling counts because a child over it on
/// `DirectML` would pass it on every job. A child on the CPU, an error it
/// reported or a protocol violation does not count.
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

/// The kind of a failure, in fixed words for the log.
fn failure_kind(error: &SidecarError) -> Cow<'static, str> {
    Cow::Borrowed(match error {
        SidecarError::Spawn { .. } => "it could not start",
        SidecarError::Pipe(_) => "a pipe to it failed",
        SidecarError::Protocol(_) => "it broke the protocol",
        SidecarError::Crashed { .. } => "it died",
        SidecarError::Timeout { .. } => "it did not answer in time",
        SidecarError::MemoryCeiling { .. } => "it passed the memory ceiling",
        SidecarError::Remote(_) => "it reported an error",
        SidecarError::DiarizerLoad(load) => {
            return Cow::Owned(format!(
                "it could not load the diarizer's models: {}",
                failure_kind(load)
            ));
        }
        SidecarError::NotUtf8 { .. } => "a model path is not UTF-8",
    })
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
    /// The running child's pid, 0 for none.
    pid: AtomicU32,
    spawns: AtomicU64,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Option<SidecarProcess>> {
        self.process.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Installs the assets under [`Install::Allowed`]; under [`Install::Never`]
    /// a missing file is [`SpeechError::NotInstalled`]. Blocking, and called
    /// before the lock is taken.
    fn install(&self) -> Result<(), SpeechError> {
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

    /// Whether the next load asks for `DirectML`.
    fn wants_directml(&self) -> bool {
        cfg!(windows) && self.config.options.directml && !directml_switched_off()
    }

    /// Makes sure a child runs with its models loaded, replacing one that
    /// failed since its last request. If the `DirectML` probe ended a child, no
    /// audio was sent yet, so a new child loads on the CPU within the same
    /// call. The caller holds the lock.
    fn ensure_loaded(&self, slot: &mut Option<SidecarProcess>) -> Result<(), SpeechError> {
        self.drop_failed(slot, true);
        if slot.as_ref().is_some_and(|p| p.provider.is_some()) {
            return Ok(());
        }
        self.install()?;
        Ok(self.load(slot, self.wants_directml())?)
    }

    /// Kills a child that failed since its last request; an overrun counts
    /// against `DirectML` when `counts`.
    fn drop_failed(&self, slot: &mut Option<SidecarProcess>, counts: bool) {
        if let Some(process) = slot.as_mut()
            && let Some(error) = process.failed_while_idle()
        {
            if !matches!(error, SidecarError::MemoryCeiling { .. }) {
                process.provider = None;
                process.load_asked_directml = false;
            }
            self.kill_unless_reported_counting(slot, error, counts);
        }
    }

    /// The running child, or a new one when none runs.
    fn running<'a>(
        &self,
        slot: &'a mut Option<SidecarProcess>,
    ) -> Result<&'a mut SidecarProcess, SidecarError> {
        let process = if let Some(process) = slot.take() {
            process
        } else {
            let process = SidecarProcess::spawn(&self.config)?;
            self.spawns.fetch_add(1, Ordering::SeqCst);
            self.pid.store(process.pid, Ordering::SeqCst);
            process
        };
        Ok(slot.insert(process))
    }

    /// Has the running child, or a new one, load the models; one the probe
    /// ended is replaced by a child on the CPU.
    fn load(&self, slot: &mut Option<SidecarProcess>, directml: bool) -> Result<(), SidecarError> {
        let process = self.running(slot)?;
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
                let error = self.kill_unless_reported(slot, error);
                if probe_ended {
                    // `DirectML` is off now, so this cannot loop.
                    return self.load(slot, false);
                }
                Err(error)
            }
        }
    }

    /// Kills the child unless `error` came from the child itself; returns the
    /// error, logged in full at debug only.
    fn kill_unless_reported(
        &self,
        slot: &mut Option<SidecarProcess>,
        error: SidecarError,
    ) -> SidecarError {
        self.kill_unless_reported_counting(slot, error, true)
    }

    /// [`kill_unless_reported`](Self::kill_unless_reported), counting against
    /// `DirectML` only when `counts`.
    fn kill_unless_reported_counting(
        &self,
        slot: &mut Option<SidecarProcess>,
        error: SidecarError,
        counts: bool,
    ) -> SidecarError {
        if !error.reported_by_the_child() {
            if let Some(mut process) = slot.take() {
                let status = process.kill();
                tracing::warn!(
                    pid = process.pid,
                    ?status,
                    reason = %failure_kind(&error),
                    "speech sidecar killed"
                );
                tracing::debug!(pid = process.pid, %error, "the speech sidecar's error");
                if counts
                    && ended_on_directml(&error, process.provider, process.load_asked_directml)
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

    /// Diarizes `payload` in the running child or a new one, loading the
    /// diarizer first when needed; the caller holds the lock. A child without
    /// speech's models is stopped after the call.
    fn diarize(
        &self,
        slot: &mut Option<SidecarProcess>,
        models: DiarizerModels,
        sample_count: u64,
        payload: Vec<u8>,
        timeout: Duration,
    ) -> Result<Vec<SpeakerCluster>, SidecarError> {
        self.drop_failed(slot, false);
        let result = self
            .load_and_diarize(slot, models, sample_count, payload, timeout)
            .map_err(|error| self.kill_unless_reported_counting(slot, error, false));
        if let Some(process) = slot.take_if(|p| p.provider.is_none()) {
            self.pid.store(0, Ordering::SeqCst);
            process.shut_down(self.config.control_timeout);
        }
        result
    }

    fn load_and_diarize(
        &self,
        slot: &mut Option<SidecarProcess>,
        models: DiarizerModels,
        sample_count: u64,
        payload: Vec<u8>,
        timeout: Duration,
    ) -> Result<Vec<SpeakerCluster>, SidecarError> {
        let process = self.running(slot)?;
        if process.diarizer.as_ref() != Some(&models) {
            process.diarizer = None;
            let files = models.clone();
            process
                .request(
                    |id| Request::LoadDiarizer {
                        id,
                        segmentation: files.segmentation,
                        embedding: files.embedding,
                        intra_threads: files.intra_threads,
                    },
                    Vec::new(),
                    self.config.load_timeout,
                    "diarizer loaded",
                    |reply| match reply {
                        Reply::DiarizerLoaded { .. } => Ok(()),
                        other => Err(other),
                    },
                )
                // Told apart from a failed run, so the caller can check the
                // files.
                .map_err(|error| SidecarError::DiarizerLoad(Box::new(error)))?;
            tracing::info!(pid = process.pid, "speech sidecar loaded the diarizer");
            process.diarizer = Some(models);
        }
        process.request(
            |id| Request::Diarize { id, sample_count },
            payload,
            timeout,
            "a diarization",
            |reply| match reply {
                Reply::Diarization { clusters, .. } => {
                    Ok(clusters.into_iter().map(SpeakerCluster::from).collect())
                }
                other => Err(other),
            },
        )
    }
}

/// Parakeet v3 on ONNX Runtime in `steno-speech-sidecar`, the default engine on
/// Linux and Windows and the fallback on macOS.
///
/// `prepare` installs the models in this process (the child never opens a
/// connection), spawns the child and has it load them; `transcribe` sends the
/// samples over the pipe. [`SpeechEngine::release`] stops the child and frees
/// its working set. [`diarize`](Self::diarize) runs the diarizer in the same
/// child.
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

    /// For tests: installs `assets` instead of the models.
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

    /// The store the models are installed into and loaded from.
    #[must_use]
    pub fn store(&self) -> &ModelStore {
        &self.shared.store
    }

    /// The running child's pid; `None` when there is none.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        Some(self.shared.pid.load(Ordering::SeqCst)).filter(|&pid| pid != 0)
    }

    /// For tests: how many children this engine has started.
    #[must_use]
    pub fn spawns(&self) -> u64 {
        self.shared.spawns.load(Ordering::SeqCst)
    }

    /// For tests: whether a fault of the running child is queued. Blocking.
    #[doc(hidden)]
    #[must_use]
    pub fn fault_queued(&self) -> bool {
        self.shared
            .lock()
            .as_ref()
            .is_some_and(|p| p.fault_queued.load(Ordering::SeqCst))
    }

    /// Asks the running child for its pid, resident set, load state and encoder
    /// provider; `None` when no child runs. Only the tests call it.
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
                Err(error) => Err(shared.kill_unless_reported(&mut slot, error)),
            }
        })
        .await??)
    }

    /// [`SpeechEngine::release`] with the child's exit status; `None` when no
    /// child ran.
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

    /// Diarizes `audio` in the child, so a crash in ONNX Runtime ends the
    /// child, not this process. The caller installs `models`. Empty audio is no
    /// clusters, without a child.
    ///
    /// # Errors
    ///
    /// [`SpeechError::Sidecar`]: [`SidecarError::DiarizerLoad`] when the child
    /// failed while loading the models, [`SidecarError::Remote`] when the run
    /// failed in a child that keeps running, [`SidecarError::NotUtf8`] for a
    /// model path the protocol cannot carry, any other variant when the child
    /// failed and was killed.
    pub async fn diarize(
        &self,
        models: DiarizerModels,
        audio: &AudioBuffer16k,
    ) -> Result<Vec<SpeakerCluster>, SpeechError> {
        if audio.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(path) = [&models.segmentation, &models.embedding]
            .into_iter()
            .find(|path| path.to_str().is_none())
        {
            return Err(SidecarError::NotUtf8 { path: path.clone() }.into());
        }
        let shared = Arc::clone(&self.shared);
        let sample_count = audio.samples.len() as u64;
        let timeout = shared.config.transcribe_timeout(audio.duration());
        let payload = protocol::encode_samples(&audio.samples);
        Ok(blocking(move || {
            shared.diarize(&mut shared.lock(), models, sample_count, payload, timeout)
        })
        .await??)
    }
}

impl Drop for SidecarSpeechEngine {
    /// Stops the child politely when nothing else holds the state, on a
    /// blocking thread inside a tokio runtime.
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
                // A runtime shutting down drops the task; the process's drop
                // then kills the child.
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
            // The download runs before the lock.
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
                    shared.kill_unless_reported(&mut slot, error),
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

    /// A bare or relative name is refused, never looked up.
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

    /// A failed diarizer load's warning says what ended it, without stderr.
    #[test]
    fn a_failed_diarizer_load_names_its_inner_kind() {
        let crashed = SidecarError::DiarizerLoad(Box::new(SidecarError::Crashed {
            status: "signal: 6".to_owned(),
            stderr: "/home/someone/models/embedding.onnx".to_owned(),
        }));
        assert_eq!(
            failure_kind(&crashed),
            "it could not load the diarizer's models: it died"
        );
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

    /// Under `Install::Never` a missing model is `NotInstalled` and nothing
    /// reaches the host; under `Install::Allowed` it is downloaded.
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

    #[cfg(unix)]
    #[tokio::test]
    async fn a_diarizer_path_the_protocol_cannot_carry_is_refused_before_any_spawn() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let embedding = dir
            .path()
            .join(std::ffi::OsStr::from_bytes(b"embedding-\xff.onnx"));
        let engine = SidecarSpeechEngine::with_assets(
            ModelStore::new(dir.path()),
            SidecarConfig::new(dir.path().join("no-such-sidecar")),
            Vec::new(),
        );
        let models = DiarizerModels {
            segmentation: dir.path().join("segmentation.onnx"),
            embedding: embedding.clone(),
            intra_threads: 1,
        };
        let error = engine
            .diarize(models, &AudioBuffer16k::silence(1.0))
            .await
            .unwrap_err();
        assert!(
            matches!(
                &error,
                SpeechError::Sidecar(SidecarError::NotUtf8 { path }) if *path == embedding
            ),
            "{error}"
        );
        assert_eq!(engine.spawns(), 0);
    }
}
