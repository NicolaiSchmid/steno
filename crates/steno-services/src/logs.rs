//! The log output of the shell and the CLI. A line never waits for stderr:
//! it is queued for one writer thread, and a full queue drops it. Every
//! line at `warn` and above carries ids, stages, counts and error kinds
//! only, never transcript text, audio, a file path or a secret; full
//! errors go to `debug`.

use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use tracing_subscriber::fmt::MakeWriter;

/// The shell's log filter when `RUST_LOG` is unset.
pub const LOG_FILTER: &str = "warn";

/// The lines the queue holds before it drops new ones: a few seconds of
/// `RUST_LOG=debug` at its busiest.
const QUEUED_LINES: usize = 4096;

/// How long [`flush_logs`] waits for the writer thread.
const FLUSH_PATIENCE: Duration = Duration::from_secs(1);

/// How long a panic waits for the writer thread before the panic message.
const PANIC_FLUSH_PATIENCE: Duration = Duration::from_millis(250);

/// The queue of the installed output, for [`flush_logs`].
static OUTPUT: OnceLock<LineQueue> = OnceLock::new();

/// Installs the log output: lines on stderr, filtered by `RUST_LOG`, else
/// by `default_filter`. A second call does nothing. A panic writes out the
/// queued lines (waiting at most a quarter of a second) before its message;
/// a panic inside [`steno_core::crash_log::expected`] does not. A line that
/// finds the queue full, or that stderr refuses, is dropped.
///
/// ```no_run
/// steno_services::log_to_stderr(steno_services::LOG_FILTER);
/// tracing::warn!("the handover identity is missing");
/// // Before `std::process::exit`, which ends the writer thread at once.
/// steno_services::flush_logs();
/// ```
pub fn log_to_stderr(default_filter: &str) {
    if OUTPUT.get().is_some() {
        return;
    }
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter));
    let queue = spawn_writer(std::io::stderr(), QUEUED_LINES);
    let installed = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(queue.clone())
        .log_internal_errors(false)
        .try_init();
    if installed.is_ok() && OUTPUT.set(queue).is_ok() {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic| {
            // A panic its caller catches and logs waits for nothing.
            if steno_core::crash_log::is_expected() {
                return;
            }
            if let Some(queue) = OUTPUT.get() {
                queue.flush_within(PANIC_FLUSH_PATIENCE);
            }
            previous(panic);
        }));
    }
}

/// Writes out the lines queued so far, waiting at most a second; a no-op
/// before [`log_to_stderr`]. Call it before `std::process::exit`, which
/// ends the writer thread at once.
pub fn flush_logs() {
    if let Some(queue) = OUTPUT.get() {
        queue.flush_within(FLUSH_PATIENCE);
    }
}

/// What the writer thread receives.
enum Message {
    Line(Vec<u8>),
    /// Answered once every line queued before it is written.
    Flush(SyncSender<()>),
}

/// The sending end of the writer thread's queue; the subscriber writes
/// each formatted line through it.
#[derive(Clone)]
struct LineQueue {
    sender: SyncSender<Message>,
    /// The lines dropped on a full queue since the writer last said so.
    dropped: Arc<AtomicUsize>,
}

impl LineQueue {
    /// Waits until the lines queued so far are written, or `patience`
    /// passed; whether they were.
    fn flush_within(&self, patience: Duration) -> bool {
        let deadline = Instant::now() + patience;
        let (done, written) = sync_channel(1);
        let mut flush = Message::Flush(done);
        // `SyncSender` has no `send_timeout`; a flush is rare enough to poll.
        while let Err(error) = self.sender.try_send(flush) {
            match error {
                TrySendError::Full(again) if Instant::now() < deadline => flush = again,
                _ => return false,
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        written
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_ok()
    }
}

impl Write for &LineQueue {
    /// Queues `line`, or drops and counts it when the queue is full; never
    /// waits.
    fn write(&mut self, line: &[u8]) -> std::io::Result<usize> {
        if let Err(TrySendError::Full(_)) = self.sender.try_send(Message::Line(line.to_vec())) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        Ok(line.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LineQueue {
    type Writer = &'a LineQueue;

    fn make_writer(&'a self) -> Self::Writer {
        self
    }
}

/// Starts the thread that writes queued lines to `output`, holding at most
/// `capacity` of them; the thread ends when every queue handle is gone.
fn spawn_writer(output: impl Write + Send + 'static, capacity: usize) -> LineQueue {
    let (sender, lines) = sync_channel(capacity);
    let dropped = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&dropped);
    std::thread::Builder::new()
        .name("steno-log".to_owned())
        .spawn(move || write_lines(output, &lines, &counted))
        .expect("the log writer thread starts");
    LineQueue { sender, dropped }
}

fn write_lines(mut output: impl Write, lines: &Receiver<Message>, dropped: &AtomicUsize) {
    while let Ok(message) = lines.recv() {
        // Everything already queued, then one flush.
        for message in std::iter::once(message).chain(lines.try_iter()) {
            match message {
                Message::Line(line) => {
                    let _ = output.write_all(&line);
                }
                Message::Flush(done) => {
                    report_dropped(&mut output, dropped);
                    let _ = output.flush();
                    let _ = done.send(());
                }
            }
        }
        report_dropped(&mut output, dropped);
        let _ = output.flush();
    }
}

/// One line for the lines dropped since the last one, if any.
fn report_dropped(output: &mut impl Write, dropped: &AtomicUsize) {
    let count = dropped.swap(0, Ordering::Relaxed);
    if count > 0 {
        let _ = writeln!(output, "steno-log: {count} lines dropped");
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use super::spawn_writer;

    /// An output that records what is written to it; its writes wait while
    /// the test holds its lock.
    #[derive(Clone, Default)]
    struct Recorded(Arc<Mutex<Vec<u8>>>);

    impl Recorded {
        fn lines(&self) -> Vec<String> {
            String::from_utf8(self.0.lock().unwrap().clone())
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect()
        }
    }

    impl Write for Recorded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().write(bytes)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// The thread that logs goes on while stderr is stuck: further lines are
    /// dropped and counted, a flush gives up at its bound, and once stderr
    /// moves the queued lines come out in order, then the drop count.
    #[test]
    fn a_stalled_output_never_blocks_the_thread_that_logs() {
        let output = Recorded::default();
        let stall = output.0.lock().unwrap();
        let queue = spawn_writer(output.clone(), 4);
        // On a thread of its own, so a send that waits fails the test.
        let (logged, done) = std::sync::mpsc::channel();
        let writer = queue.clone();
        std::thread::spawn(move || {
            let subscriber = tracing_subscriber::fmt()
                .with_writer(writer)
                .with_ansi(false)
                .without_time()
                .with_target(false)
                .with_level(false)
                .finish();
            tracing::subscriber::with_default(subscriber, || {
                for line in 0..1000 {
                    tracing::warn!("line {line}");
                }
            });
            let _ = logged.send(());
        });
        done.recv_timeout(Duration::from_secs(5))
            .expect("logging waited for the stalled output");
        assert!(!queue.flush_within(Duration::from_millis(50)));

        drop(stall);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !output
            .lines()
            .last()
            .is_some_and(|line| line.starts_with("steno-log:"))
        {
            assert!(Instant::now() < deadline, "no report: {:?}", output.lines());
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(queue.flush_within(Duration::from_secs(10)));
        // The four queued lines and the one the writer held, if any.
        let mut lines = output.lines();
        let report = lines.pop().unwrap();
        let numbers: Vec<usize> = lines
            .iter()
            .map(|line| line.strip_prefix("line ").unwrap().parse().unwrap())
            .collect();
        assert!((4..=5).contains(&numbers.len()), "{numbers:?}");
        assert_eq!(numbers[0], 0, "{numbers:?}");
        assert!(numbers.is_sorted(), "{numbers:?}");
        assert_eq!(
            report,
            format!("steno-log: {} lines dropped", 1000 - numbers.len())
        );
    }

    #[test]
    fn a_flush_answers_once_every_queued_line_is_written() {
        let output = Recorded::default();
        let queue = spawn_writer(output.clone(), 64);
        for line in 0..32 {
            // One write per line, as the subscriber writes them.
            (&queue)
                .write_all(format!("line {line}\n").as_bytes())
                .unwrap();
        }
        assert!(queue.flush_within(Duration::from_secs(10)));
        let expected: Vec<String> = (0..32).map(|line| format!("line {line}")).collect();
        assert_eq!(output.lines(), expected);
    }
}

// Unix only: there a closed terminal leaves the app running with failing
// stderr writes; on Windows it ends the process.
#[cfg(all(test, unix))]
mod closed_stderr {
    use std::process::{Command, Stdio};

    /// Set in the copy of this test binary the test runs.
    const LOGGING_CHILD: &str = "STENO_TEST_LOGGING_CHILD";

    /// A child logging to a stderr nobody reads loses the line but does not
    /// panic. `--nocapture`, or the harness would catch a panicking
    /// `eprintln!`.
    #[test]
    fn a_log_line_to_a_closed_stderr_is_dropped_without_a_panic() {
        if std::env::var_os(LOGGING_CHILD).is_some() {
            super::log_to_stderr(super::LOG_FILTER);
            tracing::warn!("a line nobody reads");
            super::flush_logs();
            return;
        }
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "logs::closed_stderr::a_log_line_to_a_closed_stderr_is_dropped_without_a_panic",
                "--nocapture",
            ])
            .env(LOGGING_CHILD, "1")
            .env_remove("RUST_LOG")
            .stdout(Stdio::piped())
            .stderr(writer)
            .output()
            .unwrap();
        let report = String::from_utf8_lossy(&child.stdout);
        assert!(child.status.success(), "{}: {report}", child.status);
        // The test ran in the child, not a filter that matched nothing.
        assert!(report.contains("1 passed"), "{report}");
    }
}
