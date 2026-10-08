//! The one synthetic-audio generator and the Int16 WAV writer: seeded
//! `SplitMix64` noise, integer phase accumulators, byte-identical on every
//! machine and to the Swift generator (`Tests/Fixtures/MANIFEST.sha256`
//! pins both). `steno dev fixtures generate` writes these files; the
//! pipeline's sample clips use the writer; `two_lane_call` (behind the
//! `testing` feature) lays the
//! conversation out as a recording for the tests across crates, and
//! `CapturedLog` captures what they log.
//! Swift: `Sources/StenoCore/Testing/FixtureGenerator.swift`,
//! `Sources/StenoCore/Audio/WAVWriter.swift`.

use std::path::Path;

use steno_core::{AudioBuffer16k, AudioLane, busy_file};
#[cfg(feature = "testing")]
pub use testing::{CapturedLog, two_lane_call};

/// One generated file: its path under the fixtures root and its SHA-256.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub relative_path: String,
    pub sha256: String,
}

const SAMPLE_RATE: f64 = AudioBuffer16k::SAMPLE_RATE;
const TWO_POW_32: f64 = 4_294_967_296.0;

/// Speaker A of the two-tone conversation: fundamental plus one harmonic.
const VOICE_A: [(f64, f64); 2] = [(330.0, 0.35), (660.0, 0.15)];
/// Speaker B.
const VOICE_B: [(f64, f64); 2] = [(220.0, 0.35), (880.0, 0.15)];

/// Every case in path order.
#[must_use]
pub fn cases() -> Vec<(&'static str, Vec<i16>)> {
    vec![
        (
            "audio/conversation-mic-6s.wav",
            conversation(6.0, &[AudioLane::Mic]),
        ),
        (
            "audio/conversation-system-6s.wav",
            conversation(6.0, &[AudioLane::System]),
        ),
        (
            "audio/conversation-two-lane-6s.wav",
            conversation(6.0, &[AudioLane::Mic, AudioLane::System]),
        ),
        ("audio/noise-2s.wav", noise(2.0, 0x5EED_0001, 0.2)),
        ("audio/sweep-3s.wav", sweep(3.0, 200.0, 4000.0, 0.5)),
    ]
}

/// Writes every case under `root` and returns the manifest entries, sorted
/// by path.
pub fn generate(root: &Path, sha256_hex: &dyn Fn(&[u8]) -> String) -> std::io::Result<Vec<Output>> {
    let mut outputs = Vec::new();
    for (relative_path, samples) in cases() {
        let path = root.join(relative_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = wav_data(&samples, 16_000, 1);
        write_atomically(&path, &data)?;
        outputs.push(Output {
            relative_path: relative_path.to_owned(),
            sha256: sha256_hex(&data),
        });
    }
    outputs.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(outputs)
}

/// `sha256sum` format: hash, two spaces, path.
#[must_use]
pub fn manifest(outputs: &[Output]) -> String {
    let mut text: String = outputs
        .iter()
        .map(|output| format!("{}  {}", output.sha256, output.relative_path))
        .collect::<Vec<_>>()
        .join("\n");
    text.push('\n');
    text
}

fn count(seconds: f64) -> usize {
    // Positive seconds at 16 kHz; exact for the fixture lengths.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let count = (seconds * SAMPLE_RATE) as usize;
    count
}

/// A linear sine sweep.
#[must_use]
pub fn sweep(seconds: f64, start: f64, end: f64, amplitude: f64) -> Vec<i16> {
    let count = count(seconds);
    let mut phase: u32 = 0;
    let mut samples = vec![0i16; count];
    for (index, sample) in samples.iter_mut().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let progress = index as f64 / (count.saturating_sub(1).max(1)) as f64;
        let frequency = start + (end - start) * progress;
        *sample = quantize(amplitude * sine(phase));
        phase = phase.wrapping_add(increment(frequency));
    }
    samples
}

/// Seeded white noise.
#[must_use]
pub fn noise(seconds: f64, seed: u64, amplitude: f64) -> Vec<i16> {
    let mut generator = SplitMix64::new(seed);
    (0..count(seconds))
        .map(|_| {
            #[allow(clippy::cast_precision_loss)]
            let unit = (generator.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
            quantize(amplitude * (unit * 2.0 - 1.0))
        })
        .collect()
}

/// Speaker A and B alternate in 1.5 s turns with 0.1 s of silence between
/// them; A speaks on `mic`, B on `system`. Passing one lane yields that
/// lane's sidecar, both lanes the mixed room recording.
#[must_use]
pub fn conversation(seconds: f64, lanes: &[AudioLane]) -> Vec<i16> {
    let count = count(seconds);
    let turn = self::count(1.5);
    let gap = self::count(0.1);
    let mut phases_a = [0u32; 2];
    let mut phases_b = [0u32; 2];
    let include_a = lanes.contains(&AudioLane::Mic) || lanes.contains(&AudioLane::Mixed);
    let include_b = lanes.contains(&AudioLane::System) || lanes.contains(&AudioLane::Mixed);
    let mut samples = vec![0i16; count];
    for (index, sample) in samples.iter_mut().enumerate() {
        let turn_index = index / turn;
        let in_gap = index % turn >= turn - gap;
        let mut value = 0.0;
        if !in_gap {
            if turn_index.is_multiple_of(2) && include_a {
                for (voice, (_, amplitude)) in VOICE_A.iter().enumerate() {
                    value += amplitude * sine(phases_a[voice]);
                }
            } else if !turn_index.is_multiple_of(2) && include_b {
                for (voice, (_, amplitude)) in VOICE_B.iter().enumerate() {
                    value += amplitude * sine(phases_b[voice]);
                }
            }
        }
        for (voice, (frequency, _)) in VOICE_A.iter().enumerate() {
            phases_a[voice] = phases_a[voice].wrapping_add(increment(*frequency));
        }
        for (voice, (frequency, _)) in VOICE_B.iter().enumerate() {
            phases_b[voice] = phases_b[voice].wrapping_add(increment(*frequency));
        }
        *sample = quantize(value);
    }
    samples
}

fn increment(frequency: f64) -> u32 {
    // A phase step below 2^32 for every audible frequency.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let step = (frequency / SAMPLE_RATE * TWO_POW_32).round() as u32;
    step
}

fn sine(phase: u32) -> f64 {
    (2.0 * std::f64::consts::PI * f64::from(phase) / TWO_POW_32).sin()
}

fn quantize(value: f64) -> i16 {
    // Clamped to the Int16 range before the cast.
    #[allow(clippy::cast_possible_truncation)]
    let sample = (value.clamp(-1.0, 1.0) * 32767.0).round() as i16;
    sample
}

/// Float samples to Int16 the way `WAVWriter.int16` did.
#[must_use]
pub fn int16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|sample| quantize(f64::from(*sample)))
        .collect()
}

/// The complete RIFF file for `samples`: `fmt ` and `data` chunks, PCM 16.
#[must_use]
pub fn wav_data(samples: &[i16], sample_rate: u32, channels: u16) -> Vec<u8> {
    let data_size = samples.len() * 2;
    let mut data = Vec::with_capacity(44 + data_size);
    data.extend_from_slice(b"RIFF");
    // The fixture files are a few hundred kilobytes; a WAV cannot exceed 4 GB.
    #[allow(clippy::cast_possible_truncation)]
    data.extend_from_slice(&((36 + data_size) as u32).to_le_bytes());
    data.extend_from_slice(b"WAVE");
    data.extend_from_slice(b"fmt ");
    data.extend_from_slice(&16u32.to_le_bytes());
    data.extend_from_slice(&1u16.to_le_bytes());
    data.extend_from_slice(&channels.to_le_bytes());
    data.extend_from_slice(&sample_rate.to_le_bytes());
    data.extend_from_slice(&(sample_rate * u32::from(channels) * 2).to_le_bytes());
    data.extend_from_slice(&(channels * 2).to_le_bytes());
    data.extend_from_slice(&16u16.to_le_bytes());
    data.extend_from_slice(b"data");
    #[allow(clippy::cast_possible_truncation)]
    data.extend_from_slice(&(data_size as u32).to_le_bytes());
    for sample in samples {
        data.extend_from_slice(&sample.to_le_bytes());
    }
    data
}

/// Writes a 16 kHz mono Int16 WAV atomically (`write_atomically`).
pub fn write_wav(path: &Path, samples: &[i16]) -> std::io::Result<()> {
    write_atomically(path, &wav_data(samples, 16_000, 1))
}

/// Writes beside `path`, its extension replaced by `wav.part`, then renames
/// that temporary over `path`, tried again on Windows while a file is busy
/// ([`busy_file::rename`]): a freshly written WAV is what an antivirus
/// client scans first. A failure removes the temporary and leaves `path` as
/// it was.
fn write_atomically(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension("wav.part");
    let written =
        std::fs::write(&temporary, data).and_then(|()| busy_file::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// The `SplitMix64` generator: tiny, seedable, identical on every platform.
#[derive(Debug, Clone)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        SplitMix64 { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

/// What only tests need: behind the `testing` feature.
#[cfg(feature = "testing")]
mod testing {
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::{Arc, Mutex, OnceLock};

    use steno_core::{
        AudioAsset, AudioFormat, AudioLane, AudioRetention, RecordingLayout, derived_uuid,
        paths::file_url,
    };
    use uuid::Uuid;

    use super::{conversation, write_wav};

    /// The six-second call recorded into `audio_folder` for `meeting_id`: the
    /// two-lane master and the mic and system sidecars of [`conversation`]
    /// (the committed `audio/conversation-*-6s.wav`), and the asset naming
    /// them, its id derived from the meeting's.
    pub fn two_lane_call(
        audio_folder: &Path,
        meeting_id: Uuid,
        retention: AudioRetention,
    ) -> std::io::Result<AudioAsset> {
        let layout = RecordingLayout::new(audio_folder, meeting_id);
        layout.create_directories(false)?;
        let master = layout.master(AudioFormat::Wav16kInt16);
        let lanes = [AudioLane::Mic, AudioLane::System];
        write_wav(&master, &conversation(6.0, &lanes))?;
        let mut sidecars_16k = BTreeMap::new();
        for lane in lanes {
            let sidecar = layout.sidecar(lane);
            write_wav(&sidecar, &conversation(6.0, &[lane]))?;
            sidecars_16k.insert(lane, file_url(&sidecar, false));
        }
        Ok(AudioAsset {
            id: derived_uuid(meeting_id, "asset"),
            meeting_id,
            url: file_url(&master, false),
            format: AudioFormat::Wav16kInt16,
            lanes: lanes.to_vec(),
            sidecars_16k,
            mixdown_url: None,
            retention,
            expires_at: None,
        })
    }

    /// What a test binary logs at `warn` and above, from the first
    /// [`CapturedLog::warnings`] on: the process-wide subscriber, so a line
    /// is captured whichever thread writes it. Every test in the binary logs
    /// into it, so a test picks its own lines out by an id of its own.
    #[derive(Clone, Default)]
    pub struct CapturedLog(Arc<Mutex<Vec<u8>>>);

    impl CapturedLog {
        /// The capture, installed on the first call.
        ///
        /// # Panics
        ///
        /// When the binary already has another global subscriber.
        pub fn warnings() -> &'static CapturedLog {
            static LOG: OnceLock<CapturedLog> = OnceLock::new();
            LOG.get_or_init(|| {
                let log = CapturedLog::default();
                tracing::subscriber::set_global_default(
                    tracing_subscriber::fmt()
                        .with_writer(log.clone())
                        .with_max_level(tracing::Level::WARN)
                        .finish(),
                )
                .expect("no other subscriber in this test binary");
                log
            })
        }

        /// Everything captured so far.
        ///
        /// # Panics
        ///
        /// When a writer panicked while holding the buffer.
        #[must_use]
        pub fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    impl std::io::Write for CapturedLog {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLog {
        type Writer = CapturedLog;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WAV that cannot be moved over its path (a folder holds the name)
    /// fails the write, leaves the folder as it was and leaves no temporary
    /// behind. On Windows the refused move is tried again first.
    #[test]
    fn a_failed_write_leaves_no_temporary_behind() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("speaker.wav");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("kept"), b"kept").unwrap();
        write_wav(&path, &[0, 1, -1]).unwrap_err();
        let names: Vec<_> = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["speaker.wav"]);
        assert_eq!(std::fs::read(path.join("kept")).unwrap(), b"kept");
    }

    /// A write that fails before its move (a folder holds the temporary's
    /// name) leaves the earlier file at `path` as it was.
    #[test]
    fn a_failed_write_keeps_the_earlier_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("speaker.wav");
        std::fs::write(&path, b"earlier").unwrap();
        std::fs::create_dir(path.with_extension("wav.part")).unwrap();
        write_wav(&path, &[0, 1, -1]).unwrap_err();
        assert_eq!(std::fs::read(&path).unwrap(), b"earlier");
    }
}
