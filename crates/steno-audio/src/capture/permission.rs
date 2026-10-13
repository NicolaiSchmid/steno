//! The system audio permission probe. macOS has no status API for the
//! system audio recording permission (`kTCCServiceAudioCapture`), so
//! onboarding runs the real tap while another process (`afplay`, which the
//! tap records like any app) plays a tone, and counts the first sample
//! above [`LaneLevel::SILENT_PEAK_LINEAR`] (-80 dBFS) as the grant. Swift:
//! `Sources/StenoAudio/Capture/SystemAudioPermission.swift`.
//!
//! The first run brings up the system prompt from inside the tap's start,
//! and the tap delivers silence until the user answers, so the probe
//! listens for up to [`PROBE_TIMEOUT`] and plays the tone again whenever it
//! has ended; only the deadline means denied. macOS attributes a tool run
//! from Terminal to Terminal, so the answer is the app's only when the
//! bundled app runs it, whose `Info.plist` carries
//! `NSAudioCaptureUsageDescription`. `tccutil reset AudioCapture
//! <bundle id>` brings the prompt back. Off the Mac there is no probe:
//! nothing there asks before a recording.
//!
//! [`LaneLevel::SILENT_PEAK_LINEAR`]: super::LaneLevel::SILENT_PEAK_LINEAR

use std::time::Duration;

use crate::clock::{Cancel, Clock};

/// How long the probe listens before it counts the permission as denied.
/// Swift: `request(timeout:)`'s default.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// How often the probe looks at what the tap delivered. Swift:
/// `SystemAudioPermission.pollInterval`.
pub const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The probe's loop, apart from the tap and the player, so a test drives it
/// on a manual clock: every `poll_interval` it calls `keep_playing` (which
/// plays the tone again once it ended), then `sample` for what the tap
/// delivered since the last call. `true` as soon as one sample is louder
/// than -80 dBFS; `false` once `timeout` has passed on `clock` without
/// one. Swift: `SystemAudioPermission.waitForSignal`.
pub fn wait_for_signal(
    timeout: Duration,
    poll_interval: Duration,
    clock: &dyn Clock,
    mut sample: impl FnMut() -> Vec<f32>,
    mut keep_playing: impl FnMut(),
) -> bool {
    let deadline = clock.now() + timeout;
    let cancel = Cancel::new();
    loop {
        keep_playing();
        if sample()
            .iter()
            .any(|value| value.abs() > super::LaneLevel::SILENT_PEAK_LINEAR)
        {
            return true;
        }
        if clock.now() >= deadline {
            return false;
        }
        clock.sleep(poll_interval, &cancel);
    }
}

/// Runs the probe for up to `timeout` (the product passes
/// [`PROBE_TIMEOUT`]) and answers whether the tap heard the tone: the
/// tap first, where the prompt appears, then the tone through
/// `/usr/bin/afplay`, played again whenever it ended. A tap or a tone that
/// cannot start answers `false`. Blocks for as long as it listens. Swift:
/// `SystemAudioPermission.request`.
#[cfg(target_os = "macos")]
pub fn probe_system_audio(timeout: Duration) -> bool {
    use std::process::{Child, Command, Stdio};
    use std::sync::Arc;

    use steno_core::AudioLane;

    use crate::capture::{CaptureBackend as _, LiveCaptureBackend};
    use crate::realtime::LaneFrameSink;

    /// Stops the player and removes the tone file however the probe ends.
    struct Cleanup {
        tone: std::path::PathBuf,
        player: Option<Child>,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Some(player) = self.player.as_mut() {
                let _ = player.kill();
                let _ = player.wait();
            }
            let _ = std::fs::remove_file(&self.tone);
        }
    }

    let tone = std::env::temp_dir().join(format!(
        "steno-permission-{}-{}.wav",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    if let Err(error) = std::fs::write(&tone, tone_wav()) {
        tracing::warn!(%error, "the system audio probe could not write its tone");
        return false;
    }
    let mut cleanup = Cleanup { tone, player: None };
    let sink = Arc::new(LaneFrameSink::new(&[AudioLane::System]));
    let backend = LiveCaptureBackend::new();
    if let Err(error) = backend.start(&[AudioLane::System], None, sink.clone()) {
        tracing::warn!(%error, "the system audio probe could not start the tap");
        return false;
    }
    let clock = crate::clock::SystemClock::new();
    let heard = wait_for_signal(
        timeout,
        POLL_INTERVAL,
        &clock,
        || sink.ring(0).drain_all(),
        || {
            let playing = cleanup
                .player
                .as_mut()
                .is_some_and(|player| matches!(player.try_wait(), Ok(None)));
            if !playing {
                cleanup.player = Command::new("/usr/bin/afplay")
                    .arg(&cleanup.tone)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .ok();
            }
        },
    );
    backend.stop();
    drop(cleanup);
    heard
}

/// One second of a 1 kHz tone at half scale, as a 48 kHz mono 16-bit WAV
/// file. Swift: `AudioFixtures.tone(frequency: 1_000, seconds: 1,
/// amplitude: 0.5)` through `AudioFixtures.writeWAV`.
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

    /// A clock whose every sleep passes at once and moves it on by the
    /// sleep's length, so the loop runs to its end on the test's thread.
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

    /// The loop over a one-second deadline polled every 100 ms, `samples`
    /// answering each poll by its number; returns the answer, the polls
    /// and the tone restarts.
    fn run(samples: impl Fn(usize) -> Vec<f32>) -> (bool, usize, usize) {
        let clock = SteppingClock::default();
        let mut polls = 0;
        let mut plays = 0;
        let granted = wait_for_signal(
            Duration::from_secs(1),
            Duration::from_millis(100),
            &clock,
            || {
                polls += 1;
                samples(polls - 1)
            },
            || plays += 1,
        );
        (granted, polls, plays)
    }

    /// One sample above -80 dBFS is the grant, however late it comes; the
    /// tone is kept playing before every look.
    #[test]
    fn the_first_audible_sample_answers_granted() {
        let (granted, polls, plays) = run(|poll| {
            if poll == 3 {
                vec![0.0, -0.001, 0.0]
            } else {
                vec![0.0; 8]
            }
        });
        assert!(granted);
        assert_eq!((polls, plays), (4, 4));
    }

    /// Silence, and sound at or below -80 dBFS, until the deadline is
    /// denied: the tap delivers zeros while the prompt is up and after a
    /// refusal.
    #[test]
    fn silence_until_the_deadline_answers_denied() {
        let (granted, polls, _) = run(|_| vec![0.0, 1e-4, -1e-4]);
        assert!(!granted);
        // At 0, 100, ..., 1000 ms: eleven looks, the last at the deadline.
        assert_eq!(polls, 11);
    }

    /// A sample at the deadline still counts.
    #[test]
    fn a_sample_at_the_deadline_counts() {
        let (granted, polls, _) = run(|poll| if poll == 10 { vec![0.5] } else { vec![] });
        assert!(granted);
        assert_eq!(polls, 11);
    }

    /// The tone is one second of 1 kHz at half scale, 48 kHz mono 16-bit.
    #[test]
    fn the_tone_is_a_second_of_a_loud_sine() {
        let file = tone_wav();
        assert_eq!(&file[..4], b"RIFF");
        assert_eq!(&file[8..12], b"WAVE");
        let samples: Vec<i16> = file[44..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| i16::from_le_bytes(*pair))
            .collect();
        assert_eq!(samples.len(), 48_000);
        let peak = samples
            .iter()
            .map(|sample| sample.unsigned_abs())
            .max()
            .unwrap();
        assert!((16_000..=16_384).contains(&peak), "{peak}");
    }
}
