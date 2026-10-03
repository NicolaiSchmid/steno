//! The parent side: spawns `steno-speech-sidecar`, supervises it and
//! implements `SpeechEngine` over it. Every request has a deadline; the
//! reader thread checks the child's heartbeat against the memory ceiling
//! and ends the current request (or the next, for an idle child) once it
//! is over; a child that dies, hangs, overruns the ceiling or breaks the
//! protocol is killed and reaped, the call returns
//! [`SpeechError::Sidecar`], and the next call starts a new child and
//! loads the models again. Nothing here can take the app down with the
//! child.

use std::collections::{BTreeSet, VecDeque};
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use steno_core::protocols::{BoundaryResult, async_trait};
use steno_core::{AudioBuffer16k, LanguageTag, RawSegment, SpeechEngine};

use super::protocol::{self, FrameError, PROTOCOL_VERSION, Reply, Request};
use crate::engine::{OnnxSpeechEngine, blocking, log_download};
use crate::error::{SidecarError, SpeechError};
use crate::model_store::{ModelAsset, ModelStore};
use crate::onnx::OnnxOptions;

/// The binary's file name, `steno-speech-sidecar` plus `.exe` on Windows.
pub const SIDECAR_BINARY: &str = if cfg!(windows) {
    "steno-speech-sidecar.exe"
} else {
    "steno-speech-sidecar"
};

/// How the client starts and limits the child.
#[derive(Debug, Clone, PartialEq)]
pub struct SidecarConfig {
    /// The `steno-speech-sidecar` binary.
    pub program: PathBuf,
    /// Arguments before the ones the client adds (`--heartbeat-ms`); the
    /// app passes none, the tests choose the fake engine and a fault.
    pub args: Vec<OsString>,
    /// Session options the child opens the models with.
    pub options: OnnxOptions,
    /// The child is killed once its resident set passes this many bytes.
    /// The fp32 export works in 2 to 3 GB; a 2 h recording adds about
    /// 0.5 GB of samples on each side of the pipe.
    pub memory_ceiling: u64,
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
    /// at 18 times real time on atlas (`RTFx` 18), so 1.0 leaves a slow
    /// laptop a wide margin.
    pub transcribe_timeout_ratio: f64,
    /// For [`Request::Health`] and the wait for [`Reply::Bye`].
    pub control_timeout: Duration,
}

impl SidecarConfig {
    /// The defaults with `program` as the binary.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        SidecarConfig {
            program: program.into(),
            args: Vec::new(),
            options: OnnxOptions::default(),
            memory_ceiling: 6 << 30,
            heartbeat: Duration::from_millis(200),
            startup_timeout: Duration::from_secs(10),
            load_timeout: Duration::from_secs(300),
            transcribe_timeout_floor: Duration::from_secs(120),
            transcribe_timeout_ratio: 1.0,
            control_timeout: Duration::from_secs(5),
        }
    }

    /// The binary beside the running executable, where the installers put
    /// it (Tauri's `externalBin`, WP8).
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
    pub pid: u32,
    pub rss_bytes: u64,
    pub loaded: bool,
}

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
    stderr: Arc<Mutex<VecDeque<String>>>,
    pid: u32,
    next_id: u64,
    loaded: bool,
    ceiling: u64,
}

/// The lines of the child's stderr kept for a crash report.
const STDERR_LINES: usize = 20;

impl SidecarProcess {
    /// Spawns the child and waits for its [`Reply::Ready`].
    fn spawn(config: &SidecarConfig) -> Result<Self, SidecarError> {
        let mut command = Command::new(&config.program);
        command
            .args(&config.args)
            .arg("--heartbeat-ms")
            .arg(config.heartbeat.as_millis().max(1).to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // The child is a console program: started from the windowed app
        // without this flag, Windows opens a console window for it on every
        // job, and closing that window kills the child mid-request.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().map_err(|source| SidecarError::Spawn {
            program: config.program.clone(),
            source,
        })?;
        let pid = child.id();
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(SidecarError::Protocol(
                "the child's pipes were not opened".to_owned(),
            ));
        };
        let (sender, events) = mpsc::channel();
        let replies = sender.clone();
        let ceiling = config.memory_ceiling;
        std::thread::Builder::new()
            .name(format!("sidecar-{pid}-stdout"))
            .spawn(move || {
                let mut stdout = BufReader::new(stdout);
                let mut over = false;
                loop {
                    let event = match protocol::read_header::<_, Reply>(&mut stdout) {
                        Ok(Some(Reply::Memory { rss_bytes })) => {
                            if over || rss_bytes <= ceiling {
                                continue;
                            }
                            over = true;
                            Event::OverCeiling(rss_bytes)
                        }
                        Ok(Some(reply)) => Event::Reply(reply),
                        Ok(None) | Err(FrameError::Truncated | FrameError::Io(_)) => Event::Closed,
                        Err(error) => Event::Garbage(error.to_string()),
                    };
                    let last = matches!(event, Event::Closed | Event::Garbage(_));
                    if replies.send(event).is_err() || last {
                        return;
                    }
                }
            })
            .map_err(SidecarError::Pipe)?;
        let tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_LINES)));
        let lines = Arc::clone(&tail);
        std::thread::Builder::new()
            .name(format!("sidecar-{pid}-stderr"))
            .spawn(move || {
                for line in BufReader::new(stderr).lines() {
                    let Ok(line) = line else { return };
                    tracing::debug!(target: "steno_speech::sidecar", pid, "{line}");
                    let mut tail = lines.lock().unwrap_or_else(PoisonError::into_inner);
                    if tail.len() == STDERR_LINES {
                        tail.pop_front();
                    }
                    tail.push_back(line);
                }
            })
            .map_err(SidecarError::Pipe)?;
        let mut process = SidecarProcess {
            child,
            stdin: Arc::new(Mutex::new(stdin)),
            events,
            sender,
            stderr: tail,
            pid,
            next_id: 1,
            loaded: false,
            ceiling,
        };
        match process.wait_for(None, config.startup_timeout)? {
            Reply::Ready { protocol, .. } if protocol == PROTOCOL_VERSION => Ok(process),
            Reply::Ready { protocol, .. } => Err(SidecarError::Protocol(format!(
                "the child speaks protocol {protocol}, this client {PROTOCOL_VERSION}"
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
                    "the sidecar closed its stdout but kept running",
                )
            }));
        };
        // The stderr reader may still be draining the last lines.
        std::thread::sleep(Duration::from_millis(50));
        let tail = self.stderr.lock().unwrap_or_else(PoisonError::into_inner);
        SidecarError::Crashed {
            status: status.to_string(),
            stderr: tail
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join("\n"),
        }
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
    fn shutdown(mut self, grace: Duration) -> Option<ExitStatus> {
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

    /// Installs the assets, then makes sure a child runs with its models
    /// loaded. Blocking; the caller holds the lock.
    fn ensure_loaded(&self, slot: &mut Option<SidecarProcess>) -> Result<(), SpeechError> {
        if slot.as_ref().is_some_and(|p| p.loaded) {
            return Ok(());
        }
        for asset in &self.assets {
            self.store.ensure(asset, &mut log_download)?;
        }
        if slot.is_none() {
            let process = SidecarProcess::spawn(&self.config)?;
            self.spawns.fetch_add(1, Ordering::SeqCst);
            self.pid.store(process.pid, Ordering::SeqCst);
            *slot = Some(process);
        }
        let process = slot.as_mut().ok_or(SpeechError::NotPrepared)?;
        let reply = process.request(
            |id| Request::Load {
                id,
                models_root: self.store.root().to_path_buf(),
                intra_threads: self.config.options.intra_threads,
                inter_threads: self.config.options.inter_threads,
            },
            Vec::new(),
            self.config.load_timeout,
            "loaded",
            |reply| match reply {
                Reply::Loaded { .. } => Ok(()),
                other => Err(other),
            },
        );
        match reply {
            Ok(()) => {
                process.loaded = true;
                Ok(())
            }
            Err(error) => Err(self.stop(slot, error).into()),
        }
    }

    /// Drops the child unless `error` came from the child itself, which
    /// then still runs and answers; returns the error.
    fn stop(&self, slot: &mut Option<SidecarProcess>, error: SidecarError) -> SidecarError {
        if !matches!(error, SidecarError::Remote(_)) {
            if let Some(mut process) = slot.take() {
                let status = process.kill();
                tracing::warn!(pid = process.pid, ?status, %error, "speech sidecar stopped");
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
/// replaced on the next call. [`SidecarSpeechEngine::release`] stops the
/// child and frees its working set; the pipeline calls it after each job.
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

    /// Installs `assets` instead of Silero and the export before the
    /// child starts; the tests pass none, as their fake child needs none.
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

    #[must_use]
    pub fn config(&self) -> &SidecarConfig {
        &self.shared.config
    }

    /// The running child's pid; `None` when there is none. Readable while
    /// a request runs.
    #[must_use]
    pub fn pid(&self) -> Option<u32> {
        Some(self.shared.pid.load(Ordering::SeqCst)).filter(|&pid| pid != 0)
    }

    /// How many children this engine has started.
    #[must_use]
    pub fn spawns(&self) -> u64 {
        self.shared.spawns.load(Ordering::SeqCst)
    }

    /// Asks the running child for its pid, resident set and whether its
    /// models are loaded; `None` when no child runs. A child that does not
    /// answer is stopped.
    pub async fn health(&self) -> BoundaryResult<Option<SidecarHealth>> {
        let shared = Arc::clone(&self.shared);
        Ok(blocking(move || {
            let mut slot = shared.lock();
            let Some(process) = slot.as_mut() else {
                return Ok(None);
            };
            process
                .request(
                    |id| Request::Health { id },
                    Vec::new(),
                    shared.config.control_timeout,
                    "health",
                    |reply| match reply {
                        Reply::Health {
                            pid,
                            rss_bytes,
                            loaded,
                            ..
                        } => Ok(SidecarHealth {
                            pid,
                            rss_bytes,
                            loaded,
                        }),
                        other => Err(other),
                    },
                )
                .map(Some)
                .map_err(|error| shared.stop(&mut slot, error))
        })
        .await??)
    }

    /// Stops the child, politely first; returns its exit status, `None`
    /// when no child ran. The next call starts a new one.
    pub async fn release(&self) -> BoundaryResult<Option<ExitStatus>> {
        let shared = Arc::clone(&self.shared);
        Ok(blocking(move || {
            let mut slot = shared.lock();
            shared.pid.store(0, Ordering::SeqCst);
            slot.take()
                .and_then(|process| process.shutdown(shared.config.control_timeout))
        })
        .await?)
    }
}

impl Drop for SidecarSpeechEngine {
    /// Stops the child politely when nothing else holds the state; the
    /// process's own drop kills it otherwise.
    fn drop(&mut self) {
        if let Some(shared) = Arc::get_mut(&mut self.shared)
            && let Some(process) = shared
                .process
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
        {
            process.shutdown(shared.config.control_timeout);
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
        blocking(move || shared.ensure_loaded(&mut shared.lock())).await??;
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
            process
                .request(
                    |id| Request::Transcribe {
                        id,
                        sample_count,
                        hint,
                    },
                    payload,
                    timeout,
                    "a transcript",
                    |reply| match reply {
                        Reply::Transcript { segments, .. } => Ok(segments),
                        other => Err(other),
                    },
                )
                .map_err(|error| SpeechError::from(shared.stop(&mut slot, error)))
        })
        .await??;
        Ok(segments)
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
        assert_eq!(engine.release().await.unwrap(), None);
    }
}
