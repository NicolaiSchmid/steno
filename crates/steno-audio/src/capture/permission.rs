//! The macOS system audio permission probe. There is no status API for
//! `kTCCServiceAudioCapture`, so the probe runs the real tap while `afplay`
//! plays a tone and counts the first sample above -80 dBFS as the grant;
//! silence until [`PROBE_TIMEOUT`] means denied. The tone is audible, so the
//! caller keeps the probe and a recording apart.

use std::time::Duration;

use crate::clock::{Cancel, Clock};

/// How long the probe listens before it counts the permission as denied.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// How often the probe looks at what the tap delivered.
pub const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// What a probe found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeAnswer {
    /// The tap heard the tone: the permission is granted.
    Heard,
    /// Only silence until the deadline: the permission is denied.
    Silent,
    /// Stopped through its [`Cancel`] before either: it found nothing.
    Stopped,
}

/// The probe's loop, apart from the tap and the player: every
/// `poll_interval` it calls `keep_playing`, then `sample`. Heard on the
/// first sample above -80 dBFS, silent once `timeout` passed, stopped once
/// `cancel` is raised.
pub fn wait_for_signal(
    timeout: Duration,
    poll_interval: Duration,
    clock: &dyn Clock,
    cancel: &Cancel,
    mut sample: impl FnMut() -> Vec<f32>,
    mut keep_playing: impl FnMut(),
) -> ProbeAnswer {
    let deadline = clock.now() + timeout;
    loop {
        if cancel.is_cancelled() {
            return ProbeAnswer::Stopped;
        }
        keep_playing();
        if sample()
            .iter()
            .any(|value| value.abs() > super::LaneLevel::SILENT_PEAK_LINEAR)
        {
            return ProbeAnswer::Heard;
        }
        if clock.now() >= deadline {
            return ProbeAnswer::Silent;
        }
        clock.sleep(poll_interval, cancel);
    }
}

/// The probe's tone, apart from `afplay`.
#[cfg(any(target_os = "macos", test))]
trait Tone {
    /// Plays the tone again unless it still plays.
    fn keep_playing(&mut self);
    /// Stops the tone and returns once it no longer plays.
    fn stop(&mut self);
}

/// [`wait_for_signal`] over a started tap. The tone stops before `stop_tap`,
/// so a tap stop stuck in the system never leaves the tone playing.
#[cfg(any(target_os = "macos", test))]
fn listen(
    timeout: Duration,
    clock: &dyn Clock,
    cancel: &Cancel,
    sample: impl FnMut() -> Vec<f32>,
    tone: &mut dyn Tone,
    stop_tap: impl FnOnce(),
) -> ProbeAnswer {
    let heard = wait_for_signal(timeout, POLL_INTERVAL, clock, cancel, sample, || {
        tone.keep_playing();
    });
    tone.stop();
    stop_tap();
    heard
}

/// The probe's tap, apart from the system's.
#[cfg(any(target_os = "macos", test))]
trait Tap {
    /// Starts listening; an error names why it could not.
    fn start(&self) -> Result<(), String>;
    /// What the tap delivered since the last call.
    fn sample(&self) -> Vec<f32>;
    /// Closes the tap.
    fn stop(&self);
}

/// Runs the probe for up to `timeout`: the tap first (where the prompt
/// appears), then the tone through `/usr/bin/afplay`. A tap or tone that
/// cannot start answers [`ProbeAnswer::Silent`]. Blocks until it answers or
/// `cancel` is raised, and returns only once the player and the tap stopped.
#[cfg(target_os = "macos")]
pub fn probe_system_audio(timeout: Duration, cancel: &Cancel) -> ProbeAnswer {
    use std::process::{Child, Command, Stdio};
    use std::sync::Arc;

    use steno_core::AudioLane;

    use crate::capture::{CaptureBackend as _, LiveCaptureBackend};
    use crate::realtime::LaneFrameSink;

    /// The tone through `afplay`; stops the player when dropped.
    struct Player {
        tone: std::path::PathBuf,
        player: Option<Child>,
    }
    impl Tone for Player {
        fn keep_playing(&mut self) {
            let playing = self
                .player
                .as_mut()
                .is_some_and(|player| matches!(player.try_wait(), Ok(None)));
            if !playing {
                self.player = Command::new("/usr/bin/afplay")
                    .arg(&self.tone)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .ok();
            }
        }

        fn stop(&mut self) {
            if let Some(mut player) = self.player.take() {
                let _ = player.kill();
                let _ = player.wait();
            }
        }
    }
    impl Drop for Player {
        fn drop(&mut self) {
            self.stop();
        }
    }

    /// The system audio tap, into a sink of its own.
    struct SystemTap {
        sink: Arc<LaneFrameSink>,
        backend: LiveCaptureBackend,
    }
    impl Tap for SystemTap {
        fn start(&self) -> Result<(), String> {
            self.backend
                .start(&[AudioLane::System], None, self.sink.clone())
                .map(drop)
                .map_err(|error| error.to_string())
        }

        fn sample(&self) -> Vec<f32> {
            self.sink.ring(0).drain_all()
        }

        fn stop(&self) {
            self.backend.stop();
        }
    }

    let tap = SystemTap {
        sink: Arc::new(LaneFrameSink::new(&[AudioLane::System])),
        backend: LiveCaptureBackend::new(),
    };
    probe_in(
        &tone_directory(),
        timeout,
        &crate::clock::SystemClock::new(),
        cancel,
        &tap,
        |tone| {
            Box::new(Player {
                tone: tone.to_path_buf(),
                player: None,
            })
        },
    )
}

/// [`probe_system_audio`] over `tap`, the tone written into `dir`. The tone
/// file is removed however the probe ends; only a process that dies during
/// a probe leaves it, for [`remove_leftover_tones`].
#[cfg(any(target_os = "macos", test))]
fn probe_in(
    dir: &std::path::Path,
    timeout: Duration,
    clock: &dyn Clock,
    cancel: &Cancel,
    tap: &dyn Tap,
    play: impl FnOnce(&std::path::Path) -> Box<dyn Tone>,
) -> ProbeAnswer {
    let file = match ToneFile::write(dir) {
        Ok(file) => file,
        Err(error) => {
            tracing::warn!(%error, "the system audio probe could not write its tone");
            return ProbeAnswer::Silent;
        }
    };
    // Declared after the file, so the player stops before the file goes.
    let mut player = play(&file.0);
    if let Err(error) = tap.start() {
        tracing::warn!(%error, "the system audio probe could not start the tap");
        return ProbeAnswer::Silent;
    }
    listen(
        timeout,
        clock,
        cancel,
        || tap.sample(),
        player.as_mut(),
        || tap.stop(),
    )
}

/// Where the probe writes its tone: the system's temporary directory.
pub fn tone_directory() -> std::path::PathBuf {
    std::env::temp_dir()
}

/// A tone file's name is `steno-permission-<pid>-<uuid>.wav`.
const TONE_PREFIX: &str = "steno-permission-";
const TONE_SUFFIX: &str = ".wav";

/// How old a tone file must be before [`remove_leftover_tones`] takes it
/// for a leftover; longer than any probe, even another process's, runs.
pub const LEFTOVER_TONE_AGE: Duration = Duration::from_secs(120);

#[cfg(any(target_os = "macos", test))]
fn tone_file_name(pid: u32, id: uuid::Uuid) -> String {
    format!("{TONE_PREFIX}{pid}-{}{TONE_SUFFIX}", id.hyphenated())
}

/// Whether `name` is exactly what `tone_file_name` makes (canonical decimal
/// pid, lowercase hyphenated v4 UUID).
fn is_tone_file_name(name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix(TONE_PREFIX)
        .and_then(|rest| rest.strip_suffix(TONE_SUFFIX))
    else {
        return false;
    };
    let Some((pid, id)) = rest.split_once('-') else {
        return false;
    };
    let pid_matches = pid
        .parse::<u32>()
        .is_ok_and(|parsed| parsed.to_string() == pid);
    let id_matches = uuid::Uuid::try_parse(id)
        .is_ok_and(|parsed| parsed.get_version_num() == 4 && parsed.hyphenated().to_string() == id);
    pid_matches && id_matches
}

/// A tone file this process wrote, removed when dropped.
#[cfg(any(target_os = "macos", test))]
struct ToneFile(std::path::PathBuf);

#[cfg(any(target_os = "macos", test))]
impl ToneFile {
    /// Writes the tone into a file of its own in `dir`; a failed write
    /// leaves no file.
    fn write(dir: &std::path::Path) -> std::io::Result<Self> {
        let file = Self(dir.join(tone_file_name(std::process::id(), uuid::Uuid::new_v4())));
        std::fs::write(&file.0, tone_wav())?;
        Ok(file)
    }
}

#[cfg(any(target_os = "macos", test))]
impl Drop for ToneFile {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.0)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::debug!(%error, path = %self.0.display(), "the probe's tone file was not removed");
        }
    }
}

/// Removes, at launch, tone files left in `dir` by a process that died
/// mid-probe: regular files named exactly as the probe names them and at
/// least [`LEFTOVER_TONE_AGE`] old. Errors are logged at debug. Blocking.
pub fn remove_leftover_tones(dir: &std::path::Path, now: std::time::SystemTime) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::debug!(%error, "the leftover tone files were not looked for");
            return;
        }
    };
    for entry in entries.flatten() {
        if !entry.file_name().to_str().is_some_and(is_tone_file_name) {
            continue;
        }
        let path = entry.path();
        // `DirEntry::metadata` does not follow a symbolic link.
        let leftover = entry.metadata().is_ok_and(|metadata| {
            metadata.is_file()
                && metadata.modified().is_ok_and(|written| {
                    now.duration_since(written)
                        .is_ok_and(|age| age >= LEFTOVER_TONE_AGE)
                })
        });
        if !leftover {
            continue;
        }
        if let Err(error) = std::fs::remove_file(&path) {
            tracing::debug!(%error, path = %path.display(), "a leftover tone file was not removed");
        }
    }
}

/// One second of a 1 kHz tone at half scale, as a 48 kHz mono 16-bit WAV.
#[cfg(any(target_os = "macos", test))]
fn tone_wav() -> Vec<u8> {
    let samples = crate::testing::AudioFixtures::tone(1_000.0, 1.0, 0.5);
    let mut file = crate::writer::WavStreamWriter::header(crate::SAMPLE_RATE as u32, samples.len());
    for sample in samples {
        // Within ±0.5, so the product fits an `i16`.
        let value = (sample * f32::from(i16::MAX)).round() as i16;
        file.extend_from_slice(&value.to_le_bytes());
    }
    file
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    /// A clock whose every sleep returns at once and moves it on.
    #[derive(Default)]
    struct SteppingClock(Mutex<Duration>);

    impl Clock for SteppingClock {
        fn now(&self) -> Duration {
            *self.0.lock().unwrap()
        }

        fn sleep(&self, duration: Duration, _cancel: &Cancel) -> bool {
            *self.0.lock().unwrap() += duration;
            true
        }
    }

    /// The loop over a one-second deadline polled every 100 ms; returns the
    /// answer, the polls and the tone restarts.
    fn run_until(
        samples: impl Fn(usize) -> Vec<f32>,
        cancel_after: Option<usize>,
    ) -> (ProbeAnswer, usize, usize) {
        let clock = SteppingClock::default();
        let cancel = Cancel::new();
        let mut polls = 0;
        let mut plays = 0;
        let answer = wait_for_signal(
            Duration::from_secs(1),
            Duration::from_millis(100),
            &clock,
            &cancel,
            || {
                polls += 1;
                if cancel_after == Some(polls) {
                    cancel.cancel();
                }
                samples(polls - 1)
            },
            || plays += 1,
        );
        (answer, polls, plays)
    }

    fn run(samples: impl Fn(usize) -> Vec<f32>) -> (ProbeAnswer, usize, usize) {
        run_until(samples, None)
    }

    /// One sample above -80 dBFS is the grant, however late it comes.
    #[test]
    fn the_first_audible_sample_answers_granted() {
        let (granted, polls, plays) = run(|poll| {
            if poll == 3 {
                vec![0.0, -0.001, 0.0]
            } else {
                vec![0.0; 8]
            }
        });
        assert_eq!(granted, ProbeAnswer::Heard);
        assert_eq!((polls, plays), (4, 4));
    }

    /// Silence, or sound at or below -80 dBFS, until the deadline is denied.
    #[test]
    fn silence_until_the_deadline_answers_denied() {
        let (granted, polls, _) = run(|_| vec![0.0, 1e-4, -1e-4]);
        assert_eq!(granted, ProbeAnswer::Silent);
        // At 0, 100, ..., 1000 ms: eleven looks, the last at the deadline.
        assert_eq!(polls, 11);
    }

    /// A sample at the deadline still counts.
    #[test]
    fn a_sample_at_the_deadline_counts() {
        let (granted, polls, _) = run(|poll| if poll == 10 { vec![0.5] } else { vec![] });
        assert_eq!(granted, ProbeAnswer::Heard);
        assert_eq!(polls, 11);
    }

    /// A raised cancel stops the probe at once without playing the tone again.
    #[test]
    fn a_cancel_stops_the_probe_before_the_next_tone() {
        let (answer, polls, plays) = run_until(|_| vec![0.0], Some(3));
        assert_eq!(answer, ProbeAnswer::Stopped);
        assert_eq!((polls, plays), (3, 3), "no tone after the cancel");

        let clock = SteppingClock::default();
        let cancel = Cancel::new();
        cancel.cancel();
        let mut plays = 0;
        let answer = wait_for_signal(
            Duration::from_secs(1),
            Duration::from_millis(100),
            &clock,
            &cancel,
            || vec![0.5],
            || plays += 1,
        );
        assert_eq!((answer, plays), (ProbeAnswer::Stopped, 0));
    }

    /// However the probe ends, the tone stops before the tap.
    #[test]
    fn the_tone_stops_before_the_tap() {
        use std::cell::RefCell;

        struct FakeTone<'a>(&'a RefCell<Vec<&'static str>>);
        impl Tone for FakeTone<'_> {
            fn keep_playing(&mut self) {
                self.0.borrow_mut().push("tone played");
            }
            fn stop(&mut self) {
                self.0.borrow_mut().push("tone stopped");
            }
        }

        for (heard_at, cancel_at, answer) in [
            (Some(2), None, ProbeAnswer::Heard),
            (None, None, ProbeAnswer::Silent),
            (None, Some(2), ProbeAnswer::Stopped),
        ] {
            let events = RefCell::new(Vec::new());
            let clock = SteppingClock::default();
            let cancel = Cancel::new();
            let mut polls = 0;
            let found = listen(
                Duration::from_millis(500),
                &clock,
                &cancel,
                || {
                    polls += 1;
                    if cancel_at == Some(polls) {
                        cancel.cancel();
                    }
                    if heard_at == Some(polls) {
                        vec![0.5]
                    } else {
                        vec![0.0]
                    }
                },
                &mut FakeTone(&events),
                || events.borrow_mut().push("tap stopped"),
            );
            assert_eq!(found, answer);
            let events = events.into_inner();
            assert_eq!(
                events[events.len() - 2..],
                ["tone stopped", "tap stopped"],
                "{answer:?}"
            );
            assert_eq!(events[0], "tone played", "{answer:?}");
        }
    }

    /// A tap that starts unless `fails` and answers poll `n` with `sample(n)`.
    struct FakeTap<F: Fn(usize) -> Vec<f32>> {
        fails: bool,
        sample: F,
        polls: Mutex<usize>,
    }

    impl<F: Fn(usize) -> Vec<f32>> Tap for FakeTap<F> {
        fn start(&self) -> Result<(), String> {
            if self.fails {
                Err("no tap".to_owned())
            } else {
                Ok(())
            }
        }

        fn sample(&self) -> Vec<f32> {
            let mut polls = self.polls.lock().unwrap();
            *polls += 1;
            (self.sample)(*polls)
        }

        fn stop(&self) {}
    }

    fn tap<F: Fn(usize) -> Vec<f32>>(fails: bool, sample: F) -> FakeTap<F> {
        FakeTap {
            fails,
            sample,
            polls: Mutex::new(0),
        }
    }

    /// A tone that notes, each time it plays, whether its file is there.
    struct SeenTone {
        file: std::path::PathBuf,
        seen: std::sync::Arc<Mutex<Vec<bool>>>,
    }

    impl Tone for SeenTone {
        fn keep_playing(&mut self) {
            self.seen.lock().unwrap().push(self.file.exists());
        }

        fn stop(&mut self) {}
    }

    /// [`probe_in`] on a stepping clock; the answer and whether the tone's
    /// file existed when the player was made and at each play.
    fn probe_into(
        dir: &std::path::Path,
        tap: &dyn Tap,
        cancel: &Cancel,
    ) -> (ProbeAnswer, bool, Vec<bool>) {
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let mut written = false;
        let answer = probe_in(
            dir,
            Duration::from_millis(500),
            &SteppingClock::default(),
            cancel,
            tap,
            |file| {
                written = file.is_file() && file.parent() == Some(dir);
                Box::new(SeenTone {
                    file: file.to_path_buf(),
                    seen: seen.clone(),
                })
            },
        );
        let seen = seen.lock().unwrap().clone();
        (answer, written, seen)
    }

    fn is_empty(dir: &std::path::Path) -> bool {
        std::fs::read_dir(dir).unwrap().next().is_none()
    }

    /// The tone's file exists while the probe listens and is gone after.
    #[test]
    fn the_tone_file_is_removed_once_the_probe_has_listened() {
        for (heard, answer) in [(true, ProbeAnswer::Heard), (false, ProbeAnswer::Silent)] {
            let dir = tempfile::tempdir().unwrap();
            let listening = tap(false, |poll| {
                if heard && poll == 3 {
                    vec![0.5]
                } else {
                    vec![0.0]
                }
            });
            let (found, written, seen) = probe_into(dir.path(), &listening, &Cancel::new());
            assert_eq!(found, answer);
            assert!(written, "{answer:?}");
            assert!(
                !seen.is_empty() && seen.iter().all(|&there| there),
                "{answer:?}"
            );
            assert!(is_empty(dir.path()), "{answer:?}: the tone file is gone");
        }
    }

    /// Only old regular files with the exact tone name are removed; near
    /// misses, fresh files, directories and symlinks stay. Case and
    /// trailing-space variants are checked by name alone, since
    /// case-insensitive file systems would map them onto the leftover.
    #[test]
    fn only_old_tone_files_named_as_the_probe_names_them_are_removed_at_launch() {
        let dir = tempfile::tempdir().unwrap();
        let now = std::time::SystemTime::now();
        let old = now - LEFTOVER_TONE_AGE;
        let id = uuid::Uuid::new_v4();
        let mut version_1 = id.hyphenated().to_string();
        version_1.replace_range(14..15, "1");
        let leftover = tone_file_name(4242, id);
        let other_leftover = tone_file_name(7, uuid::Uuid::new_v4());
        let fresh = tone_file_name(4242, uuid::Uuid::new_v4());
        let near_misses = [
            format!("{leftover}.part"),
            format!("x{leftover}"),
            leftover.replace("permission", "permissions"),
            leftover.replace("permission", "transcript"),
            leftover.replace("-4242-", "--"),
            leftover.replace("-4242-", "-04242-"),
            leftover.replace("-4242-", "-+4242-"),
            leftover.replace("-4242-", "-4242x-"),
            leftover.replace("-4242-", "-99999999999-"),
            leftover.replace(".wav", "-1.wav"),
            leftover.replace(&id.hyphenated().to_string(), &id.simple().to_string()),
            leftover.replace(&id.hyphenated().to_string(), &id.braced().to_string()),
            leftover.replace(&id.hyphenated().to_string(), &version_1),
        ];
        let folded_elsewhere = [
            format!("{leftover} "),
            leftover.replace(".wav", ".WAV"),
            leftover.replace("steno-", "Steno-"),
            leftover.replace(
                &id.hyphenated().to_string(),
                &id.hyphenated().to_string().to_uppercase(),
            ),
        ];
        assert!(is_tone_file_name(&leftover));
        for name in near_misses.iter().chain(&folded_elsewhere) {
            assert!(!is_tone_file_name(name), "{name}");
        }
        let write = |name: &str, written: std::time::SystemTime| {
            let path = dir.path().join(name);
            std::fs::write(&path, b"RIFF").unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(written)
                .unwrap();
        };
        for name in [&leftover, &other_leftover] {
            write(name, old);
        }
        for name in &near_misses {
            write(name, old);
        }
        write(&fresh, now);
        let directory = tone_file_name(4242, uuid::Uuid::new_v4());
        std::fs::create_dir(dir.path().join(&directory)).unwrap();
        let mut kept: Vec<String> = near_misses.to_vec();
        kept.extend([fresh, directory]);
        #[cfg(unix)]
        {
            let link = tone_file_name(4242, uuid::Uuid::new_v4());
            std::os::unix::fs::symlink(dir.path().join(&near_misses[0]), dir.path().join(&link))
                .unwrap();
            kept.push(link);
        }

        remove_leftover_tones(dir.path(), now);

        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        kept.sort();
        assert_eq!(left, kept);
    }
}
