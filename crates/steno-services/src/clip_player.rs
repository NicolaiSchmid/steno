//! The host's `ClipPlayer`: a speaker's sample clip, or a turn's span of
//! the meeting's recording cut to a private temporary WAV, played by the
//! platform's own player in a child process, one at a time, behind the
//! playback gate, so a recording that starts stops it.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use steno_audio::codec::SymphoniaAudioCodec;
use steno_audio::{Playback, PlaybackPermit};
use steno_host::services::ClipPlayer;

/// The clip under way: its file, the player and the gate's permit, which
/// must live as long as the player, so a recording that starts stops it.
struct Playing {
    clip: PathBuf,
    child: Child,
    permit: Option<PlaybackPermit>,
}

impl Playing {
    fn end(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

type Slot = Arc<Mutex<Option<Playing>>>;

fn lock(slot: &Slot) -> MutexGuard<'_, Option<Playing>> {
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Ends the clip in `slot`, if any, outside the slot's lock.
fn end(slot: &Slot) {
    let playing = lock(slot).take();
    if let Some(playing) = playing {
        playing.end();
    }
}

pub struct ProcessClipPlayer {
    playback: Playback,
    command: fn(&Path) -> Command,
    slot: Slot,
    /// The turn span cut last, removed when the next is cut and on drop.
    span: Mutex<Option<PathBuf>>,
}

impl ProcessClipPlayer {
    /// The platform's player behind the process's gate.
    #[must_use]
    pub fn new() -> Self {
        Self::with(Playback::global(), platform_command)
    }

    /// A player that runs `command` behind `playback`, for the tests.
    #[must_use]
    pub fn with(playback: Playback, command: fn(&Path) -> Command) -> Self {
        ProcessClipPlayer {
            playback,
            command,
            slot: Slot::default(),
            span: Mutex::new(None),
        }
    }
}

impl Drop for ProcessClipPlayer {
    fn drop(&mut self) {
        end(&self.slot);
        let span = self
            .span
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(span) = span {
            let _ = std::fs::remove_file(span);
        }
    }
}

/// The folder turn spans are cut into: in the temporary folder, readable by
/// this user only.
fn span_directory() -> std::io::Result<PathBuf> {
    let directory = std::env::temp_dir().join(format!("steno-turn-audio-{}", user_tag()));
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(&directory)?;
    Ok(directory)
}

/// Keeps two users' folders apart on a shared temporary folder.
fn user_tag() -> String {
    let variable = if cfg!(windows) { "USERNAME" } else { "USER" };
    std::env::var(variable)
        .ok()
        .filter(|name| !name.is_empty() && name.chars().all(char::is_alphanumeric))
        .unwrap_or_else(|| "user".to_owned())
}

impl Default for ProcessClipPlayer {
    fn default() -> Self {
        Self::new()
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
        end(&self.slot);
        if !clip.is_file() {
            return false;
        }
        let stopped = self.slot.clone();
        let Ok(permit) = self.playback.begin(move || end(&stopped)) else {
            return false;
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
            return false;
        }
        // Gone when a recording started since: the permit goes with it.
        if let Some(playing) = lock(&self.slot).as_mut() {
            playing.permit = Some(permit);
        }
        true
    }

    fn play_span(&self, recording: &Path, start: f64, end: f64) -> Option<PathBuf> {
        let directory = match span_directory() {
            Ok(directory) => directory,
            Err(error) => {
                tracing::warn!(%error, "the folder for a turn's audio could not be made");
                return None;
            }
        };
        let span = directory.join(format!("turn-{}.wav", uuid::Uuid::new_v4()));
        if let Err(error) = SymphoniaAudioCodec::excerpt_path(recording, &span, start, end) {
            tracing::warn!(%error, "a turn's audio could not be cut from the recording");
            return None;
        }
        let previous = self
            .span
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(span.clone());
        // After `play` ended the previous player, which Windows needs to
        // remove its file.
        let played = self.play(&span);
        if let Some(previous) = previous {
            let _ = std::fs::remove_file(previous);
        }
        played.then_some(span)
    }

    fn stop(&self) {
        end(&self.slot);
    }

    /// The clip under way; a player that has exited ends it here.
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
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn sleeper(_clip: &Path) -> Command {
        let mut command = Command::new("sleep");
        command.arg("30");
        command
    }

    fn quick(_clip: &Path) -> Command {
        Command::new("true")
    }

    fn clip() -> tempfile::NamedTempFile {
        tempfile::NamedTempFile::new().unwrap()
    }

    #[test]
    fn a_clip_plays_until_stopped() {
        let player = ProcessClipPlayer::with(Playback::new(), sleeper);
        let clip = clip();
        assert!(player.play(clip.path()));
        assert_eq!(player.playing().as_deref(), Some(clip.path()));
        player.stop();
        assert_eq!(player.playing(), None);
    }

    /// One second of silence at 16 kHz, as a recording to cut from.
    fn recording() -> tempfile::NamedTempFile {
        let file = tempfile::Builder::new().suffix(".wav").tempfile().unwrap();
        let mut writer = steno_audio::writer::WavStreamWriter::create(file.path(), 16_000).unwrap();
        writer.write(&[0i16; 16_000]).unwrap();
        writer.finish().unwrap();
        file
    }

    #[test]
    fn a_span_plays_from_its_own_file_and_the_next_removes_it() {
        let player = ProcessClipPlayer::with(Playback::new(), sleeper);
        let recording = recording();
        let first = player.play_span(recording.path(), 0.25, 0.75).unwrap();
        assert!(first.is_file());
        assert_eq!(player.playing().as_deref(), Some(first.as_path()));
        let second = player.play_span(recording.path(), 0.0, 0.5).unwrap();
        assert!(!first.exists(), "the earlier span is removed");
        assert_eq!(player.playing().as_deref(), Some(second.as_path()));
        // A clip stops the span.
        let clip = clip();
        assert!(player.play(clip.path()));
        assert_eq!(player.playing().as_deref(), Some(clip.path()));
        drop(player);
        assert!(
            !second.exists(),
            "dropping the player removes the last span"
        );
    }

    #[test]
    fn a_missing_recording_plays_no_span() {
        let player = ProcessClipPlayer::with(Playback::new(), sleeper);
        assert_eq!(
            player.play_span(Path::new("/nonexistent/master.caf"), 0.0, 1.0),
            None
        );
        assert_eq!(player.playing(), None);
    }

    #[test]
    fn a_missing_clip_does_not_play() {
        let player = ProcessClipPlayer::with(Playback::new(), sleeper);
        assert!(!player.play(Path::new("/nonexistent/clip.wav")));
        assert_eq!(player.playing(), None);
    }

    #[test]
    fn a_finished_clip_is_no_longer_playing() {
        let player = ProcessClipPlayer::with(Playback::new(), quick);
        let clip = clip();
        assert!(player.play(clip.path()));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while player.playing().is_some() {
            assert!(std::time::Instant::now() < deadline, "the clip never ended");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}
