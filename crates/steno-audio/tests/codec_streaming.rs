//! The streaming decoder against the whole-file decoder it replaced
//! (`tests/whole_file/`, kept verbatim): every lane, every mixdown and
//! every error must come out bit for bit the same. The inputs cover what
//! the decoder meets: the recording writer's two-lane 48 kHz CAF with its
//! sidecars, an unfinished master, CAF (Float32) and WAV (16-bit and
//! float) at 8, 16, 22.05, 44.1, 48 and 96 kHz, mono and stereo, lengths around the FIR's
//! 480-sample frame and the decoder's 32 768-frame block, an empty file,
//! the phone's AAC and MP3 fixtures (the AAC less its encoder priming, which
//! the streaming decoder drops and the whole-file one kept), and sidecars
//! that are missing, empty, unfinished or the wrong shape. With `STENO_FLEURS_DIR` set the FLEURS
//! recordings are compared too.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::too_many_lines
)]

mod whole_file;

use std::path::{Path, PathBuf};

use steno_audio::codec::{CodecError, LaneResampler, SymphoniaAudioCodec};
use steno_audio::testing::AudioFixtures;
use steno_audio::writer::{
    CafStreamWriter, LaneFrames, RecordingWriter, RecordingWriting, WavFile, WavStreamWriter,
};
use steno_core::paths::file_url;
use steno_core::{
    AudioAsset, AudioDecoder, AudioFormat, AudioLane, AudioRetention, RecordingLayout,
};
use uuid::Uuid;

/// A deterministic test signal: a tone under noise, peaking past full
/// scale now and then so the clamps are exercised.
fn signal(seed: u64, count: usize, rate: f64) -> Vec<f32> {
    let mut state = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    (0..count)
        .map(|i| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let noise = ((state >> 40) as f32 / (1u64 << 24) as f32) - 0.5;
            let tone = (2.0 * std::f64::consts::PI * 440.0 * i as f64 / rate).sin() as f32;
            let burst = if i % 7_919 < 40 { 1.6 } else { 1.0 };
            (0.6 * tone + 0.3 * noise) * burst
        })
        .collect()
}

/// `channels` as an interleaved CAF at `rate` through the crate's writer;
/// `finish` false leaves the data size at -1, `tail` appends that many
/// stray bytes after the last whole frame (a frame cut by a crash).
fn write_caf(path: &Path, rate: f64, channels: &[Vec<f32>], finish: bool, tail: usize) {
    let frames = channels[0].len();
    let mut writer = CafStreamWriter::create(path, rate, channels.len()).unwrap();
    let mut interleaved = Vec::with_capacity(frames * channels.len());
    for frame in 0..frames {
        for channel in channels {
            interleaved.push(channel[frame]);
        }
    }
    writer.write(&interleaved, frames).unwrap();
    if finish {
        writer.finish().unwrap();
    } else {
        drop(writer);
    }
    if tail > 0 {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(&vec![0x3f; tail]).unwrap();
    }
}

/// `channels` as a PCM WAV at `rate`, 16-bit integer or 32-bit float.
fn write_wav(path: &Path, rate: u32, channels: &[Vec<f32>], float: bool) {
    let frames = channels[0].len();
    let bytes_per_sample: u16 = if float { 4 } else { 2 };
    let block = bytes_per_sample * channels.len() as u16;
    let data_size = (frames * usize::from(block)) as u32;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&(if float { 3u16 } else { 1u16 }).to_le_bytes());
    bytes.extend_from_slice(&(channels.len() as u16).to_le_bytes());
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * u32::from(block)).to_le_bytes());
    bytes.extend_from_slice(&block.to_le_bytes());
    bytes.extend_from_slice(&(bytes_per_sample * 8).to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_size.to_le_bytes());
    for frame in 0..frames {
        for channel in channels {
            let sample = channel[frame];
            if float {
                bytes.extend_from_slice(&sample.to_le_bytes());
            } else {
                let int = (sample.clamp(-1.0, 1.0) * 32_767.0).round() as i16;
                bytes.extend_from_slice(&int.to_le_bytes());
            }
        }
    }
    std::fs::write(path, bytes).unwrap();
}

fn asset(
    master: &Path,
    format: AudioFormat,
    lanes: &[AudioLane],
    sidecars: &[(AudioLane, &Path)],
) -> AudioAsset {
    AudioAsset {
        id: Uuid::new_v4(),
        meeting_id: Uuid::new_v4(),
        url: file_url(master, false),
        format,
        lanes: lanes.to_vec(),
        sidecars_16k: sidecars
            .iter()
            .map(|(lane, path)| (*lane, file_url(path, false)))
            .collect(),
        mixdown_url: None,
        retention: AudioRetention::KeepForever,
        expires_at: None,
    }
}

fn bits(samples: &[f32]) -> Vec<u32> {
    samples.iter().map(|s| s.to_bits()).collect()
}

/// Panics unless `streamed` and `reference` are the same samples to the
/// bit, or the same error.
fn assert_same(
    what: &str,
    streamed: Result<Vec<f32>, CodecError>,
    reference: Result<Vec<f32>, CodecError>,
) {
    match (streamed, reference) {
        (Ok(streamed), Ok(reference)) => {
            assert_eq!(streamed.len(), reference.len(), "{what}: length");
            if let Some(at) = bits(&streamed)
                .iter()
                .zip(bits(&reference))
                .position(|(a, b)| *a != b)
            {
                panic!(
                    "{what}: sample {at} is {} streamed, {} whole-file",
                    streamed[at], reference[at]
                );
            }
        }
        (Err(streamed), Err(reference)) => assert_eq!(streamed, reference, "{what}: error"),
        (streamed, reference) => panic!(
            "{what}: {:?} streamed, {:?} whole-file",
            streamed.map(|s| s.len()),
            reference.map(|s| s.len())
        ),
    }
}

/// Every lane through `decode`, every channel through `decode_path` and
/// `read_channel` (one past the last too), and the mixdown, against the
/// whole-file decoder.
async fn assert_file_matches(what: &str, asset: &AudioAsset, scratch: &Path) {
    assert_decodes_like(what, asset, asset, scratch).await;
}

/// [`assert_file_matches`] with the whole-file decoder reading `reference`
/// instead of `asset`'s own master.
async fn assert_decodes_like(
    what: &str,
    asset: &AudioAsset,
    reference_asset: &AudioAsset,
    scratch: &Path,
) {
    let codec = SymphoniaAudioCodec::new();
    let master = steno_core::paths::file_url_path(&asset.url).unwrap();
    let reference_master = steno_core::paths::file_url_path(&reference_asset.url).unwrap();
    for lane in asset.lanes.iter().copied().chain([AudioLane::Mixed]) {
        assert_same(
            &format!("{what}: decode {}", lane.as_str()),
            codec
                .decode(asset, lane)
                .await
                .map(|b| b.samples)
                .map_err(|e| {
                    e.downcast_ref::<CodecError>()
                        .cloned()
                        .unwrap_or_else(|| CodecError::Io(e.to_string()))
                }),
            whole_file::decode(reference_asset, lane),
        );
    }
    for channel in 0..=asset.lanes.len() {
        assert_same(
            &format!("{what}: decode_path channel {channel}"),
            SymphoniaAudioCodec::decode_path(&master, channel, AudioLane::Mixed).map(|b| b.samples),
            whole_file::decode_path(&reference_master, channel, AudioLane::Mixed),
        );
        let streamed = SymphoniaAudioCodec::read_channel(&master, channel, AudioLane::Mixed);
        let reference = whole_file::read_channel(&reference_master, channel, AudioLane::Mixed);
        assert_eq!(
            streamed.as_ref().map(|c| c.sample_rate).ok(),
            reference.as_ref().map(|(rate, _)| *rate).ok(),
            "{what}: read_channel {channel} rate"
        );
        assert_same(
            &format!("{what}: read_channel {channel}"),
            streamed.map(|c| c.samples),
            reference.map(|(_, samples)| samples),
        );
    }
    let streamed_to = scratch.join(format!("{}-streamed.wav", Uuid::new_v4()));
    let reference_to = scratch.join(format!("{}-reference.wav", Uuid::new_v4()));
    let streamed = codec
        .mixdown(asset, &streamed_to)
        .await
        .map_err(|e| e.to_string());
    let reference = whole_file::mixdown(reference_asset, &reference_to).map_err(|e| e.to_string());
    assert_eq!(streamed, reference, "{what}: mixdown outcome");
    if reference.is_ok() {
        assert!(
            std::fs::read(&streamed_to).unwrap() == std::fs::read(&reference_to).unwrap(),
            "{what}: mixdown bytes"
        );
    } else {
        assert!(
            !streamed_to.exists(),
            "{what}: a failed mixdown leaves no file"
        );
    }
}

/// The scratch directory: under the target dir, not the system temp dir.
fn scratch() -> tempfile::TempDir {
    tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap()
}

/// Lengths around the FIR's frame (480), its group delay (95) and the
/// decoder's block (32 768 frames).
const LENGTHS: [usize; 14] = [
    0, 1, 2, 3, 95, 96, 479, 480, 481, 959, 32_767, 32_768, 32_769, 100_003,
];

#[tokio::test]
async fn caf_masters_at_48k_decode_as_before() {
    let directory = scratch();
    for frames in LENGTHS {
        let path = directory.path().join(format!("{frames}.caf"));
        let channels = [signal(1, frames, 48_000.0), signal(2, frames, 48_000.0)];
        write_caf(&path, 48_000.0, &channels, true, 0);
        let lanes = [AudioLane::Mic, AudioLane::System];
        assert_file_matches(
            &format!("48 kHz CAF, {frames} frames"),
            &asset(&path, AudioFormat::Caf48kFloat32, &lanes, &[]),
            directory.path(),
        )
        .await;
    }
}

#[tokio::test]
async fn unfinished_and_odd_rate_cafs_decode_as_before() {
    let directory = scratch();
    for (rate, channel_count, frames, finish, tail) in [
        (48_000.0, 2, 10_001, false, 3),
        (48_000.0, 2, 0, false, 5),
        (48_000.0, 1, 70_000, false, 0),
        (16_000.0, 1, 40_001, true, 0),
        (44_100.0, 1, 50_001, true, 0),
        (44_100.0, 2, 33_000, false, 7),
        (8_000.0, 1, 9_001, true, 0),
        (22_050.0, 2, 0, true, 0),
        (96_000.0, 1, 100_001, true, 0),
    ] {
        let path = directory
            .path()
            .join(format!("{rate}-{channel_count}-{frames}.caf"));
        let channels: Vec<Vec<f32>> = (0..channel_count)
            .map(|c| signal(10 + c as u64, frames, rate))
            .collect();
        write_caf(&path, rate, &channels, finish, tail);
        let lanes = [AudioLane::Mic, AudioLane::System];
        assert_file_matches(
            &format!("{rate} Hz CAF, {channel_count} ch, {frames} frames, finished {finish}"),
            &asset(
                &path,
                AudioFormat::Caf48kFloat32,
                &lanes[..channel_count],
                &[],
            ),
            directory.path(),
        )
        .await;
    }
}

#[tokio::test]
async fn wav_masters_decode_through_symphonia_as_before() {
    let directory = scratch();
    for (rate, channel_count, frames, float) in [
        (48_000, 2, 70_001, false),
        (48_000, 1, 33_333, true),
        (48_000, 1, 1, false),
        (16_000, 1, 20_001, false),
        (16_000, 2, 5_000, true),
        (44_100, 1, 44_101, false),
        (44_100, 2, 32_769, true),
        (22_050, 2, 12_345, false),
        (8_000, 1, 8_003, false),
        (96_000, 1, 96_007, true),
        (48_000, 1, 0, false),
    ] {
        let path = directory
            .path()
            .join(format!("{rate}-{channel_count}-{frames}-{float}.wav"));
        let channels: Vec<Vec<f32>> = (0..channel_count)
            .map(|c| signal(20 + c as u64, frames, f64::from(rate)))
            .collect();
        write_wav(&path, rate, &channels, float);
        let lanes = [AudioLane::Mic, AudioLane::System];
        assert_file_matches(
            &format!("{rate} Hz WAV, {channel_count} ch, {frames} frames, float {float}"),
            &asset(&path, AudioFormat::M4aAac, &lanes[..channel_count], &[]),
            directory.path(),
        )
        .await;
    }
    let garbage = directory.path().join("garbage.wav");
    std::fs::write(&garbage, [0x41u8; 4_000]).unwrap();
    assert_file_matches(
        "not audio",
        &asset(&garbage, AudioFormat::M4aAac, &[AudioLane::Mixed], &[]),
        directory.path(),
    )
    .await;
    let missing = directory.path().join("missing.m4a");
    assert_file_matches(
        "no file",
        &asset(&missing, AudioFormat::M4aAac, &[AudioLane::Mixed], &[]),
        directory.path(),
    )
    .await;
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../Tests/Fixtures/audio")
        .join(name)
}

/// The phone's MP3 decodes as before; its AAC decodes as before less
/// exactly the encoder priming its edit list declares (1 024 samples for
/// ffmpeg's encoder): the reference is the whole-file decode of the m4a
/// with those samples dropped, written as a float WAV at the m4a's rate,
/// which the whole-file decoder reads back bit for bit.
#[tokio::test]
async fn the_phones_aac_and_mp3_decode_as_before() {
    const PRIMING: usize = 1_024;
    let directory = scratch();
    let mp3 = fixture("tone-440-44k1-500ms.mp3");
    assert_file_matches(
        "tone-440-44k1-500ms.mp3",
        &asset(&mp3, AudioFormat::M4aAac, &[AudioLane::Mixed], &[]),
        directory.path(),
    )
    .await;
    for name in ["tone-440-44k1-500ms.m4a", "tone-440-44k1-onset-200ms.m4a"] {
        let m4a = fixture(name);
        let (rate, samples) = whole_file::read_channel(&m4a, 0, AudioLane::Mixed).unwrap();
        let reference = directory.path().join(format!("{name}.wav"));
        write_wav(&reference, rate, &[samples[PRIMING..].to_vec()], true);
        assert_decodes_like(
            name,
            &asset(&m4a, AudioFormat::M4aAac, &[AudioLane::Mixed], &[]),
            &asset(&reference, AudioFormat::M4aAac, &[AudioLane::Mixed], &[]),
            directory.path(),
        )
        .await;
    }
}

/// The recording writer's own call, with every kind of sidecar the
/// decoder can find: finished, float, missing, unfinished (zero sizes),
/// empty, stereo and at the wrong rate; all but the first two fall back to
/// the master. (A sidecar whose length disagrees with the master's is the
/// one case the two decoders part on, on purpose: `tests/codec.rs`.)
#[tokio::test]
async fn sidecars_and_the_writers_master_decode_as_before() {
    let directory = scratch();
    let layout = RecordingLayout::new(directory.path(), Uuid::new_v4());
    let mic = signal(30, 48_000 * 3, 48_000.0);
    let system = signal(31, 48_000 * 3, 48_000.0);
    let mut writer =
        RecordingWriter::new(&layout, &[AudioLane::Mic, AudioLane::System], false).unwrap();
    for start in (0..mic.len()).step_by(480) {
        writer
            .write(&LaneFrames {
                frame_count: 480,
                lanes: &[&mic[start..start + 480], &system[start..start + 480]],
                raw_mic: None,
            })
            .unwrap();
    }
    writer.finish().unwrap();
    let files = writer.files();
    let lanes = [AudioLane::Mic, AudioLane::System];
    let finished = asset(
        &files.master,
        AudioFormat::Caf48kFloat32,
        &lanes,
        &[
            (AudioLane::Mic, &files.sidecars_16k[&AudioLane::Mic]),
            (AudioLane::System, &files.sidecars_16k[&AudioLane::System]),
        ],
    );
    assert_file_matches("the writer's call", &finished, directory.path()).await;

    let unfinished = directory.path().join("unfinished.wav");
    let mut sidecar = WavStreamWriter::create(&unfinished, 16_000).unwrap();
    sidecar.write(&vec![1_000i16; 4_801]).unwrap();
    drop(sidecar);
    assert!(WavFile::read(&unfinished).is_err());
    let empty = directory.path().join("empty.wav");
    let mut sidecar = WavStreamWriter::create(&empty, 16_000).unwrap();
    sidecar.finish().unwrap();
    let stereo = directory.path().join("stereo.wav");
    write_wav(
        &stereo,
        16_000,
        &[signal(32, 900, 16e3), signal(33, 900, 16e3)],
        false,
    );
    let wrong_rate = directory.path().join("wrong-rate.wav");
    write_wav(&wrong_rate, 48_000, &[signal(34, 900, 48e3)], false);
    let float = directory.path().join("float.wav");
    // As long as the master, so the decoder takes it.
    write_wav(&float, 16_000, &[signal(35, 48_000, 16e3)], true);
    let missing = directory.path().join("missing.wav");
    for (name, path) in [
        ("unfinished", &unfinished),
        ("empty", &empty),
        ("stereo", &stereo),
        ("wrong rate", &wrong_rate),
        ("float", &float),
        ("missing", &missing),
    ] {
        let with = asset(
            &files.master,
            AudioFormat::Caf48kFloat32,
            &lanes,
            &[(AudioLane::Mic, path.as_path())],
        );
        assert_file_matches(&format!("{name} sidecar"), &with, directory.path()).await;
        assert_same(
            &format!("{name} sidecar read"),
            WavFile::read_16k_mono(path).map_err(|e| CodecError::Io(e.to_string())),
            whole_file::read_16k_mono(path).map_err(|e| CodecError::Io(e.to_string())),
        );
    }
}

/// The FLEURS recordings (16 kHz mono 16-bit WAV, real speech) through
/// `decode_path` and the sidecar reader, when `STENO_FLEURS_DIR` points at
/// them; skipped otherwise.
#[tokio::test]
async fn the_fleurs_recordings_decode_as_before() {
    let Some(root) = std::env::var_os("STENO_FLEURS_DIR") else {
        eprintln!("skipped: set STENO_FLEURS_DIR to compare the FLEURS recordings");
        return;
    };
    let mut files: Vec<PathBuf> = walk(Path::new(&root))
        .into_iter()
        .filter(|p| p.extension().is_some_and(|x| x == "wav"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no WAV under {}", root.display());
    for path in &files {
        let what = path.display().to_string();
        assert_same(
            &what,
            SymphoniaAudioCodec::decode_path(path, 0, AudioLane::Mixed).map(|b| b.samples),
            whole_file::decode_path(path, 0, AudioLane::Mixed),
        );
        assert_same(
            &format!("{what} as a sidecar"),
            WavFile::read_16k_mono(path).map_err(|e| CodecError::Io(e.to_string())),
            whole_file::read_16k_mono(path).map_err(|e| CodecError::Io(e.to_string())),
        );
    }
    println!(
        "{} FLEURS recordings decode bit for bit as before",
        files.len()
    );
}

fn walk(directory: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(directory).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found
}

/// `AudioFixtures`' tone is the writer tests' signal; it decodes the same
/// way too (a smooth signal, where the noise above is not).
#[tokio::test]
async fn a_pure_tone_decodes_as_before() {
    let directory = scratch();
    let path = directory.path().join("tone.caf");
    let tone = AudioFixtures::tone(1_000.0, 2.0, 0.5);
    write_caf(&path, 48_000.0, &[tone.clone(), tone], true, 0);
    assert_file_matches(
        "tone",
        &asset(
            &path,
            AudioFormat::Caf48kFloat32,
            &[AudioLane::Mic, AudioLane::System],
            &[],
        ),
        directory.path(),
    )
    .await;
}

/// The resampler fed in pieces of every size, from one sample to more
/// than a block, at every rate the decoder handles: the same samples as
/// one pass over the whole lane, and `settled` never promises a sample the
/// finished lane drops.
#[test]
fn the_lane_resampler_is_cut_proof() {
    let mut cut = 0x2545_f491_4f6c_dd1du64;
    for rate in [
        8_000u32, 11_025, 16_000, 22_050, 32_000, 44_100, 48_000, 96_000,
    ] {
        for count in [
            0usize, 1, 31, 32, 33, 64, 479, 480, 481, 4_095, 4_096, 4_097, 77_777,
        ] {
            let input = signal(u64::from(rate) + count as u64, count, f64::from(rate));
            let reference = whole_file::to_16k(&input, rate);
            assert_same(
                &format!("{rate} Hz, {count} samples in one piece"),
                Ok(SymphoniaAudioCodec::to_16k(&input, rate)),
                Ok(reference.clone()),
            );
            let mut resampler = LaneResampler::new(rate);
            let mut lane = Vec::new();
            let mut handed_on = Vec::new();
            let mut at = 0;
            while at < input.len() {
                cut ^= cut << 13;
                cut ^= cut >> 7;
                cut ^= cut << 17;
                let size = [1, 2, 3, 7, 160, 479, 481, 1_152, 4_096, 40_000][(cut % 10) as usize];
                let end = (at + size).min(input.len());
                resampler.push(input[at..end].iter().copied(), &mut lane);
                at = end;
                // Hand on what is settled, as the mixdown does.
                let settled = resampler.settled() - handed_on.len();
                handed_on.extend(lane.drain(..settled));
            }
            resampler.finish(&mut lane);
            handed_on.extend(lane);
            assert_same(
                &format!("{rate} Hz, {count} samples in pieces"),
                Ok(handed_on),
                Ok(reference),
            );
        }
    }
}
