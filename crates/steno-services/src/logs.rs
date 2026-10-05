//! The log output of the shell and the CLI: [`log_to_stderr`] installs it,
//! [`flush_logs`] writes out what is queued before the process exits.
//!
//! A log line never waits for stderr. The thread that logs formats it and
//! queues it for one writer thread, and a full queue drops the line (the
//! writer reports how many when it catches up): a write that blocks (a
//! stalled disk, a full pipe nobody reads) would otherwise stop whichever
//! thread logged, a capture thread included, and every other thread that
//! logs behind it on the stderr lock. Only log lines are queued: the
//! shell's `stderr_line!` and the CLI's progress lines still write to
//! stderr directly, so they can wait, and a queued line can come out after
//! a later one of theirs.
//!
//! Privacy rule for every line at `warn` and above: ids, stages, counts and
//! error kinds only, never transcript or model text, audio, a file path or
//! a secret. Full error text goes to `debug`.
//!
//! Swift: `Logger(subsystem:category:)`, the system log, which never waits
//! on stderr either.

use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use tracing_subscriber::fmt::MakeWriter;

/// The shell's log filter when `RUST_LOG` is unset.
pub const LOG_FILTER: &str = "warn";

/// The lines the queue holds before it drops new ones: a few seconds of
/// `RUST_LOG=debug` at its busiest. The bound is in lines, not bytes, so
/// long lines (a debug line's full error text) can make it tens of
/// megabytes.
const QUEUED_LINES: usize = 4096;

/// How long [`flush_logs`] waits for the writer thread.
const FLUSH_PATIENCE: Duration = Duration::from_secs(1);

/// How long a panic waits for the writer thread before the panic message.
const PANIC_FLUSH_PATIENCE: Duration = Duration::from_millis(250);

/// The queue of the installed output, for [`flush_logs`].
static OUTPUT: OnceLock<LineQueue> = OnceLock::new();

/// Installs the log output of the shell and the CLI: lines on stderr,
/// filtered by `RUST_LOG`, else by `default_filter` (the shell passes
/// [`LOG_FILTER`]), so what the services warn about (no keychain, no
/// handover identity, a re-run or re-export that failed in the background)
/// is seen. A second call does nothing. A panic writes out the queued
/// lines (waiting at most a quarter of a second) before its message, so
/// the lines that explain a crash are not lost with it.
///
/// A line that cannot be written is dropped: one that finds the queue full
/// (see the module doc), and one stderr refuses (after a closed terminal
/// every write fails).
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
            if let Some(queue) = OUTPUT.get() {
                queue.flush_within(PANIC_FLUSH_PATIENCE);
            }
            previous(panic);
        }));
    }
}

/// Writes out the lines queued so far, waiting at most a second for a
/// stalled stderr; a no-op before [`log_to_stderr`]. The shell and the CLI
/// call it before the process exits, since `std::process::exit` takes the
/// writer thread down with whatever it still holds.
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
    use std::time::Duration;

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

    /// The thread that logs goes on while stderr is stuck: the queue fills,
    /// further lines are dropped and counted, a flush gives up at its
    /// bound, and once stderr moves again the queued lines come out in
    /// order, then how many were dropped.
    #[test]
    fn a_stalled_output_never_blocks_the_thread_that_logs() {
        let output = Recorded::default();
        let stall = output.0.lock().unwrap();
        let queue = spawn_writer(output.clone(), 4);
        // On a thread of its own, so a send that waits fails the test
        // rather than hanging it.
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
        assert!(queue.flush_within(Duration::from_secs(10)));
        // The four queued lines and the one the writer held when it
        // stalled, if it had taken one by then; the rest were dropped.
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

// Unix only: there a closed terminal no longer ends the app (the shell's
// SIGHUP asks for Quit), so its writes to stderr fail; on Windows a closed
// console ends the process, and a release build has no console.
#[cfg(all(test, unix))]
mod closed_stderr {
    use std::process::{Command, Stdio};

    /// Set in the copy of this test binary the test runs.
    const LOGGING_CHILD: &str = "STENO_TEST_LOGGING_CHILD";

    /// The child logs a warning to a stderr nobody reads (a pipe with no
    /// reader fails each write, as a closed terminal does), so the line
    /// is lost; the child must still pass, not panic. `--nocapture`, or
    /// the test harness would catch an `eprintln!` that panics.
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
