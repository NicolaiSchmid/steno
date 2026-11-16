//! The host's `ClipPlayer`: a speaker's sample clip, or a turn's span of
//! the meeting's recording cut to a WAV in the app's private
//! paragraph-audio folder, played by the platform's own player in a child
//! process, one at a time, behind the playback gate, so a recording that
//! starts stops it.
//!
//! A span is cut on a thread of its own, so the host's lock is never held
//! through a decode. Every play and every stop cancels a cut under way,
//! which then plays nothing and leaves no file. A span's file goes when it
//! stops, when it ends and on any failure; the folder is emptied when the
//! player is built, so a crash leaves nothing past the next launch.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use steno_audio::codec::{CodecError, SymphoniaAudioCodec};
use steno_audio::{Playback, PlaybackPermit};
use steno_host::services::ClipPlayer;

/// How often a span's thread looks whether its player ended.
const END_POLL: Duration = Duration::from_millis(200);

/// The clip under way: its file, the player and the gate's permit, which
/// must live as long as the player, so a recording that starts stops it.
/// Dropping it ends the player and removes a span's file.
struct Playing {
    clip: PathBuf,
    child: Child,
    permit: Option<PlaybackPermit>,
    /// A span cut for this play, removed with it.
    owned: bool,
}

impl Drop for Playing {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // After the wait: Windows removes no file a process has open.
        if self.owned {
            remove(&self.clip);
        }
    }
}

fn remove(path: &Path) {
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, "a paragraph's audio could not be removed");
    }
}

type Slot = Arc<Mutex<Option<Playing>>>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Ends the clip in `slot`, if any, outside the slot's lock.
fn end(slot: &Slot) {
    let playing = lock(slot).take();
    drop(playing);
}

type Changed = Arc<dyn Fn() + Send + Sync>;

/// What the player and its span threads share.
struct Shared {
    playback: Playback,
    command: fn(&Path) -> Command,
    slot: Slot,
    /// Moved by every play and every stop. A span's thread cuts and starts
    /// only while it still holds the number it was given, and starts under
    /// this lock, so a stop that comes after finds the player to end.
    generation: Mutex<u64>,
    /// The paragraph-audio folder.
    directory: PathBuf,
    /// Told when a span started or ended on its own thread.
    changed: Mutex<Option<Changed>>,
    /// Cuts stopped part-way by a play or a stop, for the tests.
    #[cfg(test)]
    cancelled_cuts: std::sync::atomic::AtomicUsize,
}

impl Shared {
    /// A new generation, the earlier cut cancelled and the clip ended.
    /// The guard is held while the caller starts its own.
    fn next(&self) -> (MutexGuard<'_, u64>, u64) {
        let mut generation = lock(&self.generation);
        *generation += 1;
        let current = *generation;
        end(&self.slot);
        (generation, current)
    }

    fn is_current(&self, generation: u64) -> bool {
        *lock(&self.generation) == generation
    }

    fn changed(&self) {
        let changed = lock(&self.changed).clone();
        if let Some(changed) = changed {
            changed();
        }
    }

    /// Starts `clip` behind the gate; `owned` hands the file to the play,
    /// which removes it when it ends. Called with the generation's lock
    /// held. False when the clip could not start; an owned clip is then
    /// already removed.
    fn start(&self, clip: &Path, owned: bool) -> bool {
        let refused = || {
            if owned {
                remove(clip);
            }
            false
        };
        if !clip.is_file() {
            return refused();
        }
        let stopped = self.slot.clone();
        let Ok(permit) = self.playback.begin(move || end(&stopped)) else {
            return refused();
        };
        // Started and kept under the gate's lock, so a recording that
        // starts meanwhile finds the clip to stop.
        let started = permit.start(|| {
            let child = (self.command)(clip)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            match child {
                Ok(child) => {
                    *lock(&self.slot) = Some(Playing {
                        clip: clip.to_owned(),
                        child,
                        permit: None,
                        owned,
                    });
                    true
                }
                Err(error) => {
                    tracing::warn!(%error, "the clip player could not start");
                    false
                }
            }
        });
        if started != Ok(true) {
            // Not spawned, or refused by a recording that started.
            return refused();
        }
        // Gone when a recording started since: the permit goes with it.
        if let Some(playing) = lock(&self.slot).as_mut() {
            playing.permit = Some(permit);
        }
        true
    }

    /// The clip under way; a player that has exited is ended here.
    fn playing(&self) -> Option<PathBuf> {
        let mut slot = lock(&self.slot);
        let running = slot
            .as_mut()
            .is_some_and(|playing| matches!(playing.child.try_wait(), Ok(None)));
        if running {
            return slot.as_ref().map(|playing| playing.clip.clone());
        }
        let finished = slot.take();
        drop(slot);
        drop(finished);
        None
    }

    /// The span thread's body: cuts `start..end` of `recording` to `span`
    /// while `generation` is current, plays it, and waits for it to end so
    /// its file goes at once.
    fn cut_and_play(&self, generation: u64, recording: &Path, span: &Path, start: f64, end: f64) {
        let cut = SymphoniaAudioCodec::excerpt_path_unless(recording, span, start, end, &|| {
            !self.is_current(generation)
        });
        match cut {
            Ok(()) => {}
            Err(CodecError::Cancelled) => {
                #[cfg(test)]
                self.cancelled_cuts
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                remove(span);
                return;
            }
            Err(error) => {
                tracing::warn!(%error, "a turn's audio could not be cut from the recording");
                remove(span);
                self.changed();
                return;
            }
        }
        let started = {
            let current = lock(&self.generation);
            if *current == generation {
                self.start(span, true)
            } else {
                remove(span);
                return;
            }
        };
        self.changed();
        if !started {
            return;
        }
        while self.playing().as_deref() == Some(span) {
            std::thread::sleep(END_POLL);
        }
        self.changed();
    }
}

pub struct ProcessClipPlayer {
    shared: Arc<Shared>,
}

impl ProcessClipPlayer {
    /// The platform's player behind the process's gate, cutting spans into
    /// `directory` ([`steno_core::StenoPaths::paragraph_audio_directory`]),
    /// which is emptied now.
    #[must_use]
    pub fn new(directory: PathBuf) -> Self {
        Self::with(Playback::global(), platform_command, directory)
    }

    /// A player that runs `command` behind `playback`, for the tests.
    #[must_use]
    pub fn with(playback: Playback, command: fn(&Path) -> Command, directory: PathBuf) -> Self {
        sweep(&directory);
        ProcessClipPlayer {
            shared: Arc::new(Shared {
                playback,
                command,
                slot: Slot::default(),
                generation: Mutex::new(0),
                directory,
                changed: Mutex::new(None),
                #[cfg(test)]
                cancelled_cuts: std::sync::atomic::AtomicUsize::new(0),
            }),
        }
    }

    /// `changed` is called, from the span's own thread, once a span
    /// started, could not be cut or start, and ended: the shell asks the
    /// host for a pass, which publishes what plays.
    pub fn on_change(&self, changed: impl Fn() + Send + Sync + 'static) {
        *lock(&self.shared.changed) = Some(Arc::new(changed));
    }
}

impl Drop for ProcessClipPlayer {
    fn drop(&mut self) {
        let (generation, _) = self.shared.next();
        drop(generation);
    }
}

/// Empties the paragraph-audio folder of what an earlier process left;
/// a folder that fails [`private_directory`]'s check is left alone.
fn sweep(directory: &Path) {
    if !directory.exists() {
        return;
    }
    if let Err(error) = check_private(directory) {
        tracing::warn!(%error, "the paragraph-audio folder is not this user's alone; not emptied");
        return;
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let removed = if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(error) = removed {
            tracing::warn!(%error, "a leftover paragraph's audio could not be removed");
        }
    }
}

/// Makes `directory`, readable by this user only (0700 on Unix), and
/// checks it: a folder that was already there with other permissions,
/// another owner, or that is not a folder is refused, never used.
fn private_directory(directory: &Path) -> std::io::Result<()> {
    if let Some(parent) = directory.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    if let Err(error) = builder.create(directory)
        && error.kind() != std::io::ErrorKind::AlreadyExists
    {
        return Err(error);
    }
    check_private(directory)
}

#[cfg(unix)]
fn check_private(directory: &Path) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    use std::os::unix::fs::MetadataExt as _;
    let metadata = std::fs::symlink_metadata(directory)?;
    if !metadata.file_type().is_dir() {
        return Err(Error::new(ErrorKind::InvalidInput, "not a folder"));
    }
    if metadata.uid() != nix::unistd::geteuid().as_raw() {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "the folder belongs to another user",
        ));
    }
    if metadata.mode() & 0o777 != 0o700 {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            format!(
                "the folder's permissions are {:o}, not 700",
                metadata.mode() & 0o777
            ),
        ));
    }
    Ok(())
}

/// The per-user cache folder is this user's already.
#[cfg(not(unix))]
fn check_private(directory: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(directory)?.file_type().is_dir() {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a folder",
        ))
    }
}

/// The command that plays `clip` on this platform.
fn platform_command(clip: &Path) -> Command {
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("afplay");
        command.arg(clip);
        command
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut command = Command::new("powershell.exe");
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "(New-Object System.Media.SoundPlayer $env:STENO_CLIP).PlaySync()",
            ])
            .env("STENO_CLIP", clip)
            .creation_flags(CREATE_NO_WINDOW);
        command
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let mut command = Command::new("pw-play");
        command.arg(clip);
        command
    }
}

impl ClipPlayer for ProcessClipPlayer {
    fn play(&self, clip: &Path) -> bool {
        let (_generation, _) = self.shared.next();
        self.shared.start(clip, false)
    }

    /// Cancels what is cut or played, and cuts the span on a thread of its
    /// own, which plays it once cut unless a play or a stop came first.
    /// `None` when the folder cannot be made or is refused, while a
    /// recording runs, or when the thread cannot start.
    fn play_span(&self, recording: &Path, start: f64, end: f64) -> Option<PathBuf> {
        let (guard, generation) = self.shared.next();
        drop(guard);
        if self.shared.playback.is_recording() {
            return None;
        }
        if let Err(error) = private_directory(&self.shared.directory) {
            tracing::warn!(%error, "the folder for a paragraph's audio cannot be used");
            return None;
        }
        let span = self
            .shared
            .directory
            .join(format!("turn-{}.wav", uuid::Uuid::new_v4()));
        let shared = self.shared.clone();
        let (recording, file) = (recording.to_owned(), span.clone());
        let spawned = std::thread::Builder::new()
            .name("steno-paragraph-audio".to_owned())
            .spawn(move || shared.cut_and_play(generation, &recording, &file, start, end));
        match spawned {
            Ok(_) => Some(span),
            Err(error) => {
                tracing::warn!(%error, "a paragraph's audio could not be prepared");
                None
            }
        }
    }

    fn stop(&self) {
        let (generation, _) = self.shared.next();
        drop(generation);
    }

    /// The clip under way; a span still being cut is not playing yet.
    fn playing(&self) -> Option<PathBuf> {
        self.shared.playing()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::Instant;

    use super::*;

    fn sleeper(_clip: &Path) -> Command {
        let mut command = Command::new("sleep");
        command.arg("30");
        command
    }

    fn quick(_clip: &Path) -> Command {
        Command::new("true")
    }

    fn missing(_clip: &Path) -> Command {
        Command::new("/nonexistent/player")
    }

    fn clip() -> tempfile::NamedTempFile {
        tempfile::NamedTempFile::new().unwrap()
    }

    struct Fixture {
        cache: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            Fixture {
                cache: tempfile::tempdir().unwrap(),
            }
        }

        fn directory(&self) -> PathBuf {
            self.cache.path().join("paragraph-audio")
        }

        fn player(&self, command: fn(&Path) -> Command) -> ProcessClipPlayer {
            ProcessClipPlayer::with(Playback::new(), command, self.directory())
        }

        /// What the folder holds.
        fn files(&self) -> Vec<PathBuf> {
            std::fs::read_dir(self.directory()).map_or_else(
                |_| Vec::new(),
                |entries| entries.flatten().map(|entry| entry.path()).collect(),
            )
        }
    }

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "{what} never happened");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// `seconds` of silence at 16 kHz, as a recording to cut from.
    fn recording(seconds: usize) -> tempfile::NamedTempFile {
        let file = tempfile::Builder::new().suffix(".wav").tempfile().unwrap();
        let mut writer = steno_audio::writer::WavStreamWriter::create(file.path(), 16_000).unwrap();
        writer.write(&vec![0i16; 16_000 * seconds]).unwrap();
        writer.finish().unwrap();
        file
    }

    #[test]
    fn a_clip_plays_until_stopped() {
        let fixture = Fixture::new();
        let player = fixture.player(sleeper);
        let clip = clip();
        assert!(player.play(clip.path()));
        assert_eq!(player.playing().as_deref(), Some(clip.path()));
        player.stop();
        assert_eq!(player.playing(), None);
        assert!(clip.path().exists(), "a speaker's clip is not the player's");
    }

    #[test]
    fn the_folder_is_made_this_users_alone() {
        let fixture = Fixture::new();
        let player = fixture.player(sleeper);
        let recording = recording(1);
        let span = player.play_span(recording.path(), 0.25, 0.75).unwrap();
        assert_eq!(span.parent(), Some(fixture.directory().as_path()));
        let mode = std::fs::metadata(fixture.directory())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        wait_until("the span playing", || {
            player.playing().as_deref() == Some(span.as_path())
        });
    }

    #[test]
    fn a_folder_open_to_others_is_refused_and_left_alone() {
        let fixture = Fixture::new();
        std::fs::create_dir(fixture.directory()).unwrap();
        std::fs::set_permissions(fixture.directory(), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let left = fixture.directory().join("someone-elses.wav");
        std::fs::write(&left, b"x").unwrap();
        let player = fixture.player(sleeper);
        assert!(left.exists(), "the sweep leaves a folder it refuses alone");
        let recording = recording(1);
        assert_eq!(player.play_span(recording.path(), 0.0, 0.5), None);
        assert_eq!(fixture.files(), vec![left]);

        // A link to a private folder is refused too.
        let fixture = Fixture::new();
        let target = fixture.cache.path().join("elsewhere");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&target, fixture.directory()).unwrap();
        let player = fixture.player(sleeper);
        assert_eq!(player.play_span(recording.path(), 0.0, 0.5), None);
    }

    #[test]
    fn the_folder_is_emptied_when_the_player_is_built() {
        let fixture = Fixture::new();
        std::fs::create_dir(fixture.directory()).unwrap();
        std::fs::set_permissions(fixture.directory(), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        std::fs::write(fixture.directory().join("turn-left.wav"), b"x").unwrap();
        let _player = fixture.player(sleeper);
        assert_eq!(fixture.files(), Vec::<PathBuf>::new());
    }

    #[test]
    fn a_span_is_removed_when_stopped_or_replaced_and_when_a_clip_plays() {
        let fixture = Fixture::new();
        let player = fixture.player(sleeper);
        let recording = recording(1);
        let first = player.play_span(recording.path(), 0.25, 0.75).unwrap();
        wait_until("the first span playing", || {
            player.playing().as_deref() == Some(first.as_path())
        });
        assert!(first.is_file());
        let second = player.play_span(recording.path(), 0.0, 0.5).unwrap();
        assert!(!first.exists(), "the earlier span is removed");
        wait_until("the second span playing", || {
            player.playing().as_deref() == Some(second.as_path())
        });
        player.stop();
        assert!(!second.exists(), "a stopped span is removed");

        let third = player.play_span(recording.path(), 0.0, 0.5).unwrap();
        wait_until("the third span playing", || player.playing().is_some());
        let clip = clip();
        assert!(player.play(clip.path()));
        assert!(!third.exists(), "a clip ends the span and removes it");
        drop(player);
        assert_eq!(fixture.files(), Vec::<PathBuf>::new());
    }

    #[test]
    fn a_span_that_ends_is_removed_and_reported() {
        let fixture = Fixture::new();
        let player = fixture.player(quick);
        let changes = Arc::new(Mutex::new(0usize));
        let counted = changes.clone();
        player.on_change(move || *lock(&counted) += 1);
        let recording = recording(1);
        let span = player.play_span(recording.path(), 0.0, 0.5).unwrap();
        wait_until("the span ending", || *lock(&changes) >= 2 && !span.exists());
        assert_eq!(player.playing(), None);
        assert_eq!(fixture.files(), Vec::<PathBuf>::new());
    }

    #[test]
    fn a_span_that_fails_leaves_no_file() {
        let fixture = Fixture::new();
        // The player does not start.
        let player = fixture.player(missing);
        let changes = Arc::new(Mutex::new(0usize));
        let counted = changes.clone();
        player.on_change(move || *lock(&counted) += 1);
        let recording = recording(1);
        let span = player.play_span(recording.path(), 0.0, 0.5).unwrap();
        wait_until("the failure reported", || *lock(&changes) >= 1);
        assert!(!span.exists());
        assert_eq!(player.playing(), None);

        // The recording does not decode.
        let broken = tempfile::Builder::new().suffix(".wav").tempfile().unwrap();
        std::fs::write(broken.path(), b"RIFF not a wave").unwrap();
        let player = fixture.player(sleeper);
        let changes = Arc::new(Mutex::new(0usize));
        let counted = changes.clone();
        player.on_change(move || *lock(&counted) += 1);
        player.play_span(broken.path(), 0.0, 0.5).unwrap();
        wait_until("the failure reported", || *lock(&changes) >= 1);
        assert_eq!(fixture.files(), Vec::<PathBuf>::new());
        assert_eq!(player.playing(), None);
    }

    /// A long recording, so the cut is still under way when the stop or
    /// the next play comes: the cancelled span never plays and leaves no
    /// file.
    #[test]
    fn a_stop_or_another_play_cancels_a_span_being_cut() {
        let fixture = Fixture::new();
        let player = fixture.player(sleeper);
        let long = recording(120);
        let first = player.play_span(long.path(), 0.0, 60.0).unwrap();
        player.stop();
        let short = recording(1);
        let second = player.play_span(short.path(), 0.0, 0.5).unwrap();
        wait_until("the second span playing", || {
            player.playing().as_deref() == Some(second.as_path())
        });
        let clip = clip();
        let third = player.play_span(long.path(), 0.0, 60.0).unwrap();
        assert!(player.play(clip.path()));
        let cancelled = &player.shared.cancelled_cuts;
        wait_until("both long cuts cancelled part-way", || {
            cancelled.load(std::sync::atomic::Ordering::SeqCst) == 2
        });
        assert_eq!(player.playing().as_deref(), Some(clip.path()));
        for span in [&first, &second, &third] {
            assert!(!span.exists(), "{}", span.display());
        }
        assert_eq!(fixture.files(), Vec::<PathBuf>::new());
    }

    #[test]
    fn a_missing_recording_plays_no_span() {
        let fixture = Fixture::new();
        let player = fixture.player(sleeper);
        let changes = Arc::new(Mutex::new(0usize));
        let counted = changes.clone();
        player.on_change(move || *lock(&counted) += 1);
        player.play_span(Path::new("/nonexistent/master.caf"), 0.0, 1.0);
        wait_until("the failure reported", || *lock(&changes) >= 1);
        assert_eq!(player.playing(), None);
        assert_eq!(fixture.files(), Vec::<PathBuf>::new());
    }

    #[test]
    fn a_missing_clip_does_not_play() {
        let fixture = Fixture::new();
        let player = fixture.player(sleeper);
        assert!(!player.play(Path::new("/nonexistent/clip.wav")));
        assert_eq!(player.playing(), None);
    }

    #[test]
    fn a_finished_clip_is_no_longer_playing() {
        let fixture = Fixture::new();
        let player = fixture.player(quick);
        let clip = clip();
        assert!(player.play(clip.path()));
        wait_until("the clip ending", || player.playing().is_none());
        assert!(clip.path().exists());
    }
}
