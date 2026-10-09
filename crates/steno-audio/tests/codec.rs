//! Decode and mixdown on files built in setup: a two-channel 48 kHz CAF
//! from the recording writer, a 16 kHz WAV master, and the sinc resampler
//! on a 44.1 kHz tone; plus the phone path's containers on committed
//! synthetic fixtures in `Tests/Fixtures/audio/` (ffmpeg encodes of half a
//! second of a 440 Hz sine, `tone-440-44k1-500ms.{m4a,mp3}`; of the same
//! sine after 0.2 s of silence, mono and with a second channel,
//! `tone-440-44k1-onset-200ms.m4a` and `tone-440-1000-44k1-stereo-onset.m4a`;
//! and two files from Apple's encoder in the phone recorder's layout,
//! `tone-440-44k1-onset-200ms-apple.m4a` and
//! `silence-44k1-avaudiorecorder.m4a`; there is no AAC or MP3 encoder in
//! pure Rust to build them in setup).
//! Swift: `Tests/StenoAudioTests/AVFoundationAudioCodecTests.swift`.

// Test arithmetic: sample counts and dB values cast freely, and sample
// rates compare exactly on purpose.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::too_many_lines,
    clippy::doc_markdown,
    clippy::cast_lossless
)]

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use steno_audio::EchoMetrics;
use steno_audio::codec::priming::{MAX_PRIMING_FRAMES, Priming, PrimingSource};
use steno_audio::codec::sinc::SincResampler;
use steno_audio::codec::{CodecError, SymphoniaAudioCodec};
use steno_audio::testing::AudioFixtures;
use steno_audio::writer::{
    LaneFrames, RecordingFiles, RecordingWriter, RecordingWriting, WavFile, WavStreamWriter,
};
use steno_core::paths::file_url;
use steno_core::{
    AudioAsset, AudioDecoder, AudioFormat, AudioLane, AudioRetention, RecordingLayout,
};
use uuid::Uuid;

use common::{frequency, level_against_sine, onset, rms_decibels};

fn write_call(layout: &RecordingLayout, seconds: f64, finish: bool) -> RecordingWriter {
    let mic = AudioFixtures::tone(1_000.0, seconds, 0.5);
    let system = AudioFixtures::tone(1_000.0, seconds, 0.25);
    write_call_lanes(layout, &mic, &system, finish)
}

/// `mic` and `system` at 48 kHz, whole 480-sample frames, through the
/// recording writer into `layout`.
fn write_call_lanes(
    layout: &RecordingLayout,
    mic: &[f32],
    system: &[f32],
    finish: bool,
) -> RecordingWriter {
    let mut writer =
        RecordingWriter::new(layout, &[AudioLane::Mic, AudioLane::System], false).unwrap();
    for start in (0..mic.len()).step_by(480) {
        writer
            .write(&LaneFrames {
                frame_count: 480,
                lanes: &[&mic[start..start + 480], &system[start..start + 480]],
                raw_mic: None,
            })
            .unwrap();
    }
    if finish {
        writer.finish().unwrap();
    }
    writer
}

/// An asset over `master` with `sidecars` as the persist stage would
/// record it.
fn make_asset(
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

/// The asset over a two-lane call the recording writer finished.
fn call_asset(files: &RecordingFiles) -> AudioAsset {
    let sidecars: Vec<(AudioLane, &Path)> = files
        .sidecars_16k
        .iter()
        .map(|(lane, path)| (*lane, path.as_path()))
        .collect();
    make_asset(
        &files.master,
        AudioFormat::Caf48kFloat32,
        &[AudioLane::Mic, AudioLane::System],
        &sidecars,
    )
}

/// A two-lane meeting folder: 1 kHz on the mic lane at 0.5, 1 kHz on the
/// system lane at 0.25, two seconds, with sidecars.
fn make_call_asset(directory: &Path) -> AudioAsset {
    let layout = RecordingLayout::new(directory, Uuid::new_v4());
    call_asset(&write_call(&layout, 2.0, true).files())
}

/// `count` samples of a sine at 0.5, sampled at 44.1 kHz like the phone's
/// recordings.
fn tone_44k1(hertz: f64, count: usize) -> Vec<f32> {
    (0..count)
        .map(|i| 0.5 * (2.0 * std::f64::consts::PI * hertz * i as f64 / 44_100.0).sin() as f32)
        .collect()
}

#[tokio::test]
async fn decodes_each_lane_of_the_master_like_its_sidecar() {
    let directory = tempfile::tempdir().unwrap();
    let asset = make_call_asset(directory.path());
    let codec = SymphoniaAudioCodec::new();
    assert_eq!(codec.mixdown_format(), AudioFormat::Wav16kInt16);

    let mic_from_sidecar = codec.decode(&asset, AudioLane::Mic).await.unwrap();
    let system_from_sidecar = codec.decode(&asset, AudioLane::System).await.unwrap();
    let mut without_sidecars = asset.clone();
    without_sidecars.sidecars_16k = BTreeMap::new();
    let mic_from_master = codec
        .decode(&without_sidecars, AudioLane::Mic)
        .await
        .unwrap();
    let system_from_master = codec
        .decode(&without_sidecars, AudioLane::System)
        .await
        .unwrap();

    assert_eq!(mic_from_sidecar.len(), 32_000);
    assert_eq!(
        mic_from_master.len(),
        32_000,
        "exactly length x 16 000 / 48 000"
    );
    assert_eq!(system_from_master.len(), 32_000);
    let window = 4_000..30_000;
    let difference =
        |a: &[f32], b: &[f32]| rms_decibels(&a[window.clone()]) - rms_decibels(&b[window.clone()]);
    let mic_difference = difference(&mic_from_master.samples, &mic_from_sidecar.samples);
    let system_difference = difference(&system_from_master.samples, &system_from_sidecar.samples);
    assert!(
        mic_difference.abs() < 0.1,
        "mic master vs sidecar {mic_difference} dB"
    );
    assert!(
        system_difference.abs() < 0.1,
        "system master vs sidecar {system_difference} dB"
    );
    assert!((EchoMetrics::rms(&mic_from_master.samples[window.clone()]) - 0.3536).abs() < 0.01);
    assert!((EchoMetrics::rms(&system_from_master.samples[window.clone()]) - 0.1768).abs() < 0.005);
    // Same samples once the sidecar's 32-sample group delay is undone (the
    // onset test below pins that shift; a 1 kHz tone alone cannot).
    let mut max_error = 0.0f32;
    for index in window {
        max_error = max_error
            .max((mic_from_master.samples[index] - mic_from_sidecar.samples[index + 32]).abs());
    }
    assert!(
        max_error < 0.02,
        "master decode vs shifted sidecar: {max_error}"
    );

    let error = codec
        .decode(&without_sidecars, AudioLane::Mixed)
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        CodecError::LaneNotInAsset(AudioLane::Mixed).to_string()
    );
}

/// The group delay compensation, checked where a shift is visible: a
/// 1 kHz tone has a 16-sample period at 16 kHz, so a 32- or 64-sample
/// error passes a sample-by-sample compare. An onset at 1.0 s on the mic
/// lane lands at 16 000 (within one) in the master decode, which drops the
/// FIR's group delay so the lane sits on the master's time like a
/// zero-phase conversion; the sidecar, written live by the same causal
/// filter, carries that delay and has the onset 32 samples (2 ms) later,
/// as Swift's did. The two decodes agree once that shift is undone.
#[tokio::test]
async fn master_decode_is_zero_phase_and_the_sidecar_lags_one_group_delay() {
    let directory = tempfile::tempdir().unwrap();
    let layout = RecordingLayout::new(directory.path(), Uuid::new_v4());
    let mut mic = vec![0.0f32; 96_000];
    mic[48_000..].copy_from_slice(&AudioFixtures::tone(1_000.0, 1.0, 0.5));
    let system = vec![0.0f32; 96_000];
    let asset = call_asset(&write_call_lanes(&layout, &mic, &system, true).files());
    let mut without_sidecars = asset.clone();
    without_sidecars.sidecars_16k = BTreeMap::new();
    let codec = SymphoniaAudioCodec::new();
    let from_sidecar = codec.decode(&asset, AudioLane::Mic).await.unwrap();
    let from_master = codec
        .decode(&without_sidecars, AudioLane::Mic)
        .await
        .unwrap();
    let sidecar_onset = onset(&from_sidecar.samples, 0.1);
    let master_onset = onset(&from_master.samples, 0.1);
    assert!(
        (15_999..=16_001).contains(&master_onset),
        "master decode onset at {master_onset}"
    );
    assert!(
        (16_031..=16_033).contains(&sidecar_onset),
        "sidecar onset at {sidecar_onset}"
    );
    assert_eq!(sidecar_onset - master_onset, 32, "one group delay apart");
    let shift = sidecar_onset - master_onset;
    let max_error = (16_100..31_000)
        .map(|i| (from_master.samples[i] - from_sidecar.samples[i + shift]).abs())
        .fold(0.0f32, f32::max);
    assert!(max_error < 0.02, "shifted decodes differ by {max_error}");
}

/// Crash recovery through the real decoder: a master whose writer never
/// reached `finish()` (data chunk size -1) reads to the last frame, and
/// sidecars with zero-size headers are skipped in favour of the master.
#[tokio::test]
async fn an_unfinished_master_with_empty_sidecars_decodes_from_the_master() {
    let directory = tempfile::tempdir().unwrap();
    let layout = RecordingLayout::new(directory.path(), Uuid::new_v4());
    // No finish(): the process "died" here, its handles still open.
    let writer = write_call(&layout, 1.5, false);
    let asset = make_asset(
        &layout.master(AudioFormat::Caf48kFloat32),
        AudioFormat::Caf48kFloat32,
        &[AudioLane::Mic, AudioLane::System],
        &[
            (AudioLane::Mic, &layout.sidecar(AudioLane::Mic)),
            (AudioLane::System, &layout.sidecar(AudioLane::System)),
        ],
    );
    assert!(
        WavFile::read(&layout.sidecar(AudioLane::Mic)).is_err(),
        "zero-size header, samples after it"
    );
    let codec = SymphoniaAudioCodec::new();
    let decoded_mic = codec.decode(&asset, AudioLane::Mic).await.unwrap();
    let decoded_system = codec.decode(&asset, AudioLane::System).await.unwrap();
    assert_eq!(decoded_mic.len(), 24_000, "1.5 s to the last frame written");
    assert_eq!(decoded_system.len(), 24_000);
    let window = 4_000..22_000;
    assert!((EchoMetrics::rms(&decoded_mic.samples[window.clone()]) - 0.3536).abs() < 0.01);
    assert!((EchoMetrics::rms(&decoded_system.samples[window]) - 0.1768).abs() < 0.005);
    // Only now may the writer go away (and with it the open handles).
    assert_eq!(writer.lanes(), &[AudioLane::Mic, AudioLane::System]);
}

#[tokio::test]
async fn mixdown_is_a_16k_mono_wav_of_the_right_length() {
    let directory = tempfile::tempdir().unwrap();
    let asset = make_call_asset(directory.path());
    let codec = SymphoniaAudioCodec::new();
    let layout = RecordingLayout::from_asset(&asset).unwrap();
    let target = layout.mixdown(codec.mixdown_format());
    codec.mixdown(&asset, &target).await.unwrap();
    assert_eq!(target.file_name().unwrap(), "audio.wav");
    let file = WavFile::read(&target).unwrap();
    assert_eq!((file.sample_rate, file.channels.len()), (16_000, 1));
    assert!((file.duration() - 2.0).abs() < 0.01);
    // Both lanes are 1 kHz in phase: the mono average is (0.5 + 0.25) / 2.
    assert!(level_against_sine(&file.channels[0][8_000..28_000], 0.375).abs() < 1.0);

    // A 16 kHz WAV asset is copied, not re-encoded.
    let wav_dir = tempfile::tempdir().unwrap();
    let wav_layout = RecordingLayout::new(wav_dir.path(), Uuid::new_v4());
    wav_layout.create_directories(false).unwrap();
    let master = wav_layout.master(AudioFormat::Wav16kInt16);
    let mut writer = WavStreamWriter::create(&master, 16_000).unwrap();
    writer.write(&vec![1_000i16; 16_000]).unwrap();
    writer.finish().unwrap();
    let wav_asset = make_asset(&master, AudioFormat::Wav16kInt16, &[AudioLane::Mixed], &[]);
    let copy = wav_layout.mixdown(AudioFormat::Wav16kInt16);
    codec.mixdown(&wav_asset, &copy).await.unwrap();
    assert_eq!(
        std::fs::read(&copy).unwrap(),
        std::fs::read(&master).unwrap()
    );
}

/// A 16 kHz WAV master decodes through symphonia to what the crate's own
/// reader sees.
#[tokio::test]
async fn wav_master_decodes_through_symphonia() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("recording.wav");
    let mut writer = WavStreamWriter::create(&path, 16_000).unwrap();
    let tone: Vec<i16> = (0..48_000)
        .map(|i| {
            ((2.0 * std::f64::consts::PI * 440.0 * i as f64 / 16_000.0).sin() * 16_000.0) as i16
        })
        .collect();
    writer.write(&tone).unwrap();
    writer.finish().unwrap();
    let asset = make_asset(&path, AudioFormat::Wav16kInt16, &[AudioLane::Mixed], &[]);
    let decoded = SymphoniaAudioCodec::new()
        .decode(&asset, AudioLane::Mixed)
        .await
        .unwrap();
    let reference = WavFile::read_16k_mono(&path).unwrap();
    assert_eq!(decoded.len(), reference.len());
    assert!(
        decoded
            .samples
            .iter()
            .zip(&reference)
            .all(|(a, b)| (a - b).abs() < 1e-4)
    );
}

/// A stereo 16-bit PCM WAV at 48 kHz, built byte by byte: the left channel
/// a 440 Hz sine at 0.5, the right a 1 kHz sine at 0.1. Every container the
/// phone path reads in the other tests is mono, so this is what pins the
/// de-interleaving: each channel decodes to its own samples, at its own
/// level and frequency, through `read_channel` and `decode_path`.
#[test]
fn a_stereo_wav_decodes_each_channel_on_its_own() {
    let frames = 24_000usize;
    let sine = |hertz: f64, amplitude: f64, i: usize| {
        ((2.0 * std::f64::consts::PI * hertz * i as f64 / 48_000.0).sin() * amplitude * 32_767.0)
            as i16
    };
    let left: Vec<i16> = (0..frames).map(|i| sine(440.0, 0.5, i)).collect();
    let right: Vec<i16> = (0..frames).map(|i| sine(1_000.0, 0.1, i)).collect();
    let data_size = (frames * 4) as u32;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&2u16.to_le_bytes()); // channels
    bytes.extend_from_slice(&48_000u32.to_le_bytes());
    bytes.extend_from_slice(&(48_000u32 * 4).to_le_bytes()); // bytes a second
    bytes.extend_from_slice(&4u16.to_le_bytes()); // bytes a frame
    bytes.extend_from_slice(&16u16.to_le_bytes()); // bits
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_size.to_le_bytes());
    for (l, r) in left.iter().zip(&right) {
        bytes.extend_from_slice(&l.to_le_bytes());
        bytes.extend_from_slice(&r.to_le_bytes());
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("stereo.wav");
    std::fs::write(&path, bytes).unwrap();

    for (channel, written, hertz, amplitude) in [(0, &left, 440.0, 0.5), (1, &right, 1_000.0, 0.1)]
    {
        let source = SymphoniaAudioCodec::read_channel(&path, channel, AudioLane::Mixed).unwrap();
        assert_eq!((source.sample_rate, source.channels), (48_000, 2));
        assert_eq!(source.samples.len(), frames, "channel {channel}");
        assert!(
            source
                .samples
                .iter()
                .zip(written.iter())
                .all(|(decoded, sample)| (decoded - f32::from(*sample) / 32_768.0).abs() < 1e-6),
            "channel {channel} decodes to its own samples"
        );
        let decoded = SymphoniaAudioCodec::decode_path(&path, channel, AudioLane::Mixed).unwrap();
        assert_eq!(decoded.len(), 8_000);
        let steady = &decoded.samples[800..7_200];
        assert!(
            level_against_sine(steady, amplitude).abs() < 0.2,
            "channel {channel} level"
        );
        assert!((frequency(steady, 16_000.0) - hertz).abs() < 5.0);
    }
    assert!(matches!(
        SymphoniaAudioCodec::read_channel(&path, 2, AudioLane::Mixed),
        Err(CodecError::ChannelMissing { channels: 2, .. })
    ));
}

/// The phone's m4a and a 16 kHz WAV declare their length in their
/// headers: half a second without the AAC priming, which the decode
/// drops too (it keeps the encoder's padding, under a packet), and six
/// seconds. A header that declares more than five hours is capped there.
#[test]
fn a_containers_declared_duration_is_read_without_decoding() {
    let path = fixture("tone-440-44k1-500ms.m4a");
    let m4a = SymphoniaAudioCodec::declared_duration(&path)
        .unwrap()
        .unwrap();
    let decoded = SymphoniaAudioCodec::read_channel(&path, 0, AudioLane::Mixed).unwrap();
    let measured = decoded.samples.len() as f64 / f64::from(decoded.sample_rate);
    assert!((m4a - 0.5).abs() < 1.0e-3, "{m4a}");
    let padding = measured - m4a;
    assert!(
        (0.0..1_024.0 / 44_100.0).contains(&padding),
        "declared {m4a}, decoded {measured}"
    );
    let wav = SymphoniaAudioCodec::declared_duration(&fixture("conversation-mic-6s.wav"))
        .unwrap()
        .unwrap();
    assert!((wav - 6.0).abs() < 0.01, "{wav}");
    let dir = tempfile::tempdir().unwrap();
    let junk = dir.path().join("recording.m4a");
    std::fs::write(&junk, b"not audio").unwrap();
    assert!(SymphoniaAudioCodec::declared_duration(&junk).is_err());
    let corrupt = dir.path().join("recording.wav");
    std::fs::write(&corrupt, wav_declaring(0x7FFF_FFF0)).unwrap();
    let capped = SymphoniaAudioCodec::declared_duration(&corrupt)
        .unwrap()
        .unwrap();
    assert!((capped - 5.0 * 3_600.0).abs() < 1.0e-6, "{capped}");
}

/// A 16 kHz mono Int16 WAV whose data chunk claims `data_bytes` and holds
/// a hundred frames of silence.
fn wav_declaring(data_bytes: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&data_bytes.saturating_add(36).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&16_000u32.to_le_bytes());
    bytes.extend_from_slice(&32_000u32.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_bytes.to_le_bytes());
    bytes.extend_from_slice(&[0; 200]);
    bytes
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../Tests/Fixtures/audio")
        .join(name)
}

/// The phone path's containers: half a second of a 440 Hz sine at 0.5,
/// mono 44.1 kHz, as AAC-LC and as MP3 (96 kbps, ffmpeg). Frequency,
/// level and the exact-length rule hold through `decode_path`, and both
/// start on the encoder's first sample: symphonia trims the MP3 priming
/// (the LAME tag, with `enable_gapless`), the decoder the AAC priming the
/// MP4 edit list declares (1 024 samples, which the lane used to start
/// with). The onset fixture below pins the AAC cut to the sample.
#[test]
fn aac_and_mp3_fixtures_decode_from_their_first_sample() {
    for name in ["tone-440-44k1-500ms.m4a", "tone-440-44k1-500ms.mp3"] {
        let path = fixture(name);
        let source = SymphoniaAudioCodec::read_channel(&path, 0, AudioLane::Mixed).unwrap();
        assert_eq!((source.sample_rate, source.channels), (44_100, 1), "{name}");
        let decoded = SymphoniaAudioCodec::decode_path(&path, 0, AudioLane::Mixed).unwrap();
        let expected_len = (source.samples.len() as f64 * 16_000.0 / 44_100.0).round() as usize;
        assert_eq!(decoded.len(), expected_len, "{name}: exact length rule");
        assert!(
            (22_050..=22_050 + 2_048).contains(&source.samples.len()),
            "{name}: {} source samples",
            source.samples.len()
        );
        let onset_source = onset(&source.samples, 0.05);
        let onset_16k = onset(&decoded.samples, 0.05);
        println!(
            "{name}: {} samples at 44.1 kHz, onset at {onset_source}; {} samples at 16 kHz, onset at {onset_16k}",
            source.samples.len(),
            decoded.len()
        );
        assert!(onset_source <= 16, "{name}: onset at {onset_source}");
        let steady = &decoded.samples[onset_16k + 400..onset_16k + 400 + 6_000];
        let level = level_against_sine(steady, 0.5);
        assert!(level.abs() < 1.0, "{name}: level {level} dB");
        let hertz = frequency(steady, 16_000.0);
        assert!((hertz - 440.0).abs() < 5.0, "{name}: {hertz} Hz");
    }
}

/// The ISO boxes `path` names (`[b"moov", b"trak", b"edts"]`) as the
/// offset of the last one's header: a walk from the file's top level.
fn box_offset(bytes: &[u8], path: &[&[u8; 4]]) -> usize {
    let (mut at, mut end) = (0, bytes.len());
    for (depth, kind) in path.iter().enumerate() {
        loop {
            assert!(at + 8 <= end, "no {} box", String::from_utf8_lossy(*kind));
            let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            if &bytes[at + 4..at + 8] == *kind {
                if depth + 1 < path.len() {
                    end = at + size;
                    at += 8;
                }
                break;
            }
            at += size;
        }
    }
    at
}

/// What a rewritten onset fixture declares of its priming.
#[derive(Debug, Clone, Copy)]
enum Declared {
    /// ffmpeg's own edit list: 1 024.
    EditList,
    /// An empty edit (a delay before the track), then ffmpeg's: 1 024.
    /// Android's `MPEG4Writer` writes that for a delayed track.
    DelayThenEditList,
    /// No edit list, iTunes' gapless tag naming this many samples: the
    /// layout of `AVAudioFile` and `afconvert`.
    Gapless(u32),
    /// ffmpeg's edit list and a gapless tag naming this many.
    EditListAndGapless(u32),
    /// An edit list starting at 0 and a gapless tag naming this many.
    ZeroEditAndGapless(u32),
    /// Neither, in this layout.
    Neither(Layout),
}

/// The marks of a rewritten fixture that declares no priming, which the
/// decoder reads to tell `AVAudioRecorder`'s files from the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// ffmpeg's own: compatible brands `M4A isom iso2`, its encoder tag
    /// in `moov/udta`, ES_ID 1, stream byte 0x15.
    Ffmpeg,
    /// `AVAudioRecorder`'s: major brand `M4A `, `mp42` for `iso2`, no
    /// `udta`, ES_ID 0, stream byte 0x14.
    Recorder,
    /// Android's `MPEG4Writer`'s (`mp42`, no `udta`, ES_ID 0, 0x15) under
    /// an `.m4a`'s major brand: the recorder's but for the stream byte.
    Android,
    /// The recorder's with major brand `mp42`.
    MajorMp42,
    /// The recorder's with `iso2` kept, so no `mp42`.
    NoMp42,
    /// The recorder's with ffmpeg's `udta` kept.
    Udta,
    /// The recorder's with ES_ID 1.
    EsId1,
}

impl Layout {
    /// The major brand, the third compatible brand, whether ffmpeg's
    /// `udta` stays, the ES_ID and the stream byte.
    fn marks(self) -> (&'static [u8; 4], &'static [u8; 4], bool, u16, u8) {
        let recorder = (b"M4A ", b"mp42", false, 0, 0x14);
        match self {
            Self::Ffmpeg => (b"M4A ", b"iso2", true, 1, 0x15),
            Self::Recorder => recorder,
            Self::Android => (recorder.0, recorder.1, false, 0, 0x15),
            Self::MajorMp42 => (b"mp42", recorder.1, false, 0, 0x14),
            Self::NoMp42 => (recorder.0, b"iso2", false, 0, 0x14),
            Self::Udta => (recorder.0, recorder.1, true, 0, 0x14),
            Self::EsId1 => (recorder.0, recorder.1, false, 1, 0x14),
        }
    }
}

/// Adds `by` to the size of the box whose header is at `at`.
fn grow(bytes: &mut [u8], at: usize, by: usize) {
    let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize + by;
    bytes[at..at + 4].copy_from_slice(&(size as u32).to_be_bytes());
}

/// The offsets of the ES_ID and the stream byte in the `esds` of an
/// ffmpeg fixture's sound track.
fn esds_fields(bytes: &[u8]) -> (usize, usize) {
    let stsd = box_offset(
        bytes,
        &[b"moov", b"trak", b"mdia", b"minf", b"stbl", b"stsd"],
    );
    // `stsd`'s header, version and flags and count; `mp4a`'s header and
    // its 28 bytes of fields.
    let esds = stsd + 16 + 8 + 28;
    assert_eq!(&bytes[esds + 4..esds + 8], b"esds");
    // A descriptor's tag, then a length whose bytes but the last have
    // their top bit set.
    let body = |tag: u8, mut at: usize| {
        assert_eq!(bytes[at], tag);
        at += 1;
        while bytes[at] & 0x80 != 0 {
            at += 1;
        }
        at + 1
    };
    // The box's header and its version and flags.
    let es_id = body(0x03, esds + 12);
    assert_eq!(bytes[es_id + 2], 0, "no optional ES fields");
    // The decoder config: the object type, then the stream byte.
    (es_id, body(0x04, es_id + 3) + 1)
}

/// `bytes`, an ffmpeg onset fixture, rewritten as `declared` says, in
/// `directory`. A dropped edit list or `udta` is renamed `free` (the box
/// stays, so no offset moves); an added edit or gapless tag grows `moov`,
/// the file's last box.
fn variant(directory: &Path, bytes: &[u8], declared: Declared) -> PathBuf {
    let mut bytes = bytes.to_vec();
    let edts = box_offset(&bytes, &[b"moov", b"trak", b"edts"]);
    // `edts`'s header, then `elst`'s, its version and flags and its count.
    let (elst, entries) = (edts + 8, edts + 8 + 8 + 4 + 4);
    assert_eq!(&bytes[elst + 4..elst + 8], b"elst");
    match declared {
        Declared::EditList | Declared::EditListAndGapless(_) => {}
        Declared::DelayThenEditList => {
            // A duration of 100 ms, the media time -1 and the rate 1.0.
            let delay = [100u32.to_be_bytes(), (-1i32).to_be_bytes(), [0, 1, 0, 0]].concat();
            bytes.splice(entries..entries, delay);
            bytes[entries - 4..entries].copy_from_slice(&2u32.to_be_bytes());
            let moov = box_offset(&bytes, &[b"moov"]);
            let trak = box_offset(&bytes, &[b"moov", b"trak"]);
            for at in [moov, trak, edts, elst] {
                grow(&mut bytes, at, 12);
            }
        }
        Declared::ZeroEditAndGapless(_) => {
            // The one entry's duration, then its media time.
            let media_time = entries + 4;
            bytes[media_time..media_time + 4].copy_from_slice(&0u32.to_be_bytes());
        }
        Declared::Gapless(_) => bytes[edts + 4..edts + 8].copy_from_slice(b"free"),
        Declared::Neither(layout) => {
            bytes[edts + 4..edts + 8].copy_from_slice(b"free");
            let (major, third, udta, es_id, stream) = layout.marks();
            assert_eq!((&bytes[4..8], &bytes[24..28]), (&b"ftyp"[..], &b"iso2"[..]));
            // `ftyp`'s header, the major brand, the minor version, then
            // the compatible brands `M4A `, `isom` and the third.
            bytes[8..12].copy_from_slice(major);
            bytes[24..28].copy_from_slice(third);
            if !udta {
                let udta = box_offset(&bytes, &[b"moov", b"udta"]);
                bytes[udta + 4..udta + 8].copy_from_slice(b"free");
            }
            let (es_id_at, stream_at) = esds_fields(&bytes);
            assert_eq!(
                (&bytes[es_id_at..es_id_at + 2], bytes[stream_at]),
                (&[0u8, 1][..], 0x15)
            );
            bytes[es_id_at..es_id_at + 2].copy_from_slice(&es_id.to_be_bytes());
            bytes[stream_at] = stream;
        }
    }
    if let Declared::Gapless(priming)
    | Declared::EditListAndGapless(priming)
    | Declared::ZeroEditAndGapless(priming) = declared
    {
        let moov = box_offset(&bytes, &[b"moov"]);
        let moov_size = u32::from_be_bytes(bytes[moov..moov + 4].try_into().unwrap()) as usize;
        assert_eq!(moov + moov_size, bytes.len(), "moov is the last box");
        let udta = gapless_udta(priming);
        grow(&mut bytes, moov, udta.len());
        bytes.extend_from_slice(&udta);
    }
    let path = directory.join(format!("{declared:?}.m4a"));
    std::fs::write(&path, bytes).unwrap();
    path
}

/// A `udta` box holding iTunes' gapless tag that names `priming` samples.
fn gapless_udta(priming: u32) -> Vec<u8> {
    let plain_box = |kind: &[u8; 4], body: &[u8]| {
        let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(body);
        b
    };
    // Version and flags, then the body.
    let full_box = |kind: &[u8; 4], body: &[u8]| plain_box(kind, &[&[0u8; 4][..], body].concat());
    let value = format!(" 00000000 {priming:08X} 00000000 0000000000005A00");
    let mut data = vec![0, 0, 0, 1, 0, 0, 0, 0];
    data.extend_from_slice(value.as_bytes());
    let item = plain_box(
        b"----",
        &[
            full_box(b"mean", b"com.apple.iTunes"),
            full_box(b"name", b"iTunSMPB"),
            plain_box(b"data", &data),
        ]
        .concat(),
    );
    let mut handler = vec![0u8; 4];
    handler.extend_from_slice(b"mdirappl");
    handler.extend_from_slice(&[0; 9]);
    let meta = full_box(
        b"meta",
        &[full_box(b"hdlr", &handler), plain_box(b"ilst", &item)].concat(),
    );
    plain_box(b"udta", &meta)
}

/// A sine at 0.5 from sample `start` of 22 050 at 44.1 kHz: what ffmpeg
/// was given for the onset fixtures.
fn sine_after(start: usize, hz: f64) -> Vec<f32> {
    (0..22_050)
        .map(|n| {
            if n < start {
                return 0.0;
            }
            let t = (n - start) as f64 / 44_100.0;
            (0.5 * (2.0 * std::f64::consts::PI * hz * t).sin()) as f32
        })
        .collect()
}

/// Each channel of `path` at its own rate, decoded by the streaming
/// decoder, until a channel is missing.
fn channels_of(path: &Path) -> Vec<Vec<f32>> {
    (0..)
        .map_while(|channel| {
            SymphoniaAudioCodec::read_channel(path, channel, AudioLane::Mixed)
                .ok()
                .map(|decoded| decoded.samples)
        })
        .collect()
}

/// The priming ffmpeg's AAC encoder adds and its edit list declares.
const PRIMING: usize = 1_024;

/// What the decoder makes of a file one past its bound: no trim.
const PAST_THE_BOUND: Declared = Declared::Gapless(4_097);

/// The cases of [`the_aac_priming_is_trimmed_to_the_sample`] and its
/// stereo sibling, each with the frames the decoder should cut: the
/// declared priming one packet (1 024), after a delay edit too; Apple's
/// 2 112 (two packets and 64 frames, a cut inside a packet), the
/// decoder's bound and one past it; nothing declared, which cuts Apple's
/// 2 112 in `AVAudioRecorder`'s layout alone; and an edit list that wins
/// over a gapless tag, even at 0.
const DECLARATIONS: [(Declared, usize); 15] = [
    (Declared::EditList, PRIMING),
    (Declared::DelayThenEditList, PRIMING),
    (Declared::Gapless(1_024), 1_024),
    (Declared::Gapless(2_112), 2_112),
    (Declared::Gapless(4_096), 4_096),
    (PAST_THE_BOUND, 0),
    (Declared::Neither(Layout::Recorder), 2_112),
    (Declared::Neither(Layout::Ffmpeg), 0),
    (Declared::Neither(Layout::Android), 0),
    (Declared::Neither(Layout::MajorMp42), 0),
    (Declared::Neither(Layout::NoMp42), 0),
    (Declared::Neither(Layout::Udta), 0),
    (Declared::Neither(Layout::EsId1), 0),
    (Declared::EditListAndGapless(2_112), PRIMING),
    (Declared::ZeroEditAndGapless(2_112), 0),
];

/// The AAC priming trimmed to the sample. The fixture is 0.2 s of silence
/// and then a 440 Hz sine at 0.5 from sample 8 820 (ffmpeg's AAC encoder,
/// which primes 1 024 samples and says so in the edit list). With the
/// edit list (after a delay edit too), or iTunes' gapless tag naming
/// 1 024 (Apple's layout), the
/// lane starts on the encoder's first sample, so the tone crosses the
/// onset threshold on the sample the PCM given to the encoder does
/// (8 822), and at 3 201 at 16 kHz. A tag naming more cuts that many
/// frames, inside a packet too; one past [`MAX_PRIMING_FRAMES`] cuts
/// nothing; a file that declares neither loses Apple's 2 112, as
/// AVFoundation assumes, only with every mark of `AVAudioRecorder`'s
/// layout, and keeps every sample in ffmpeg's, in Android's and with any
/// one mark missing; an edit list beside a tag wins, even at 0. Every
/// trimmed decode is the untrimmed one less exactly its cut, bit for bit:
/// the trim comes from the container.
#[test]
fn the_aac_priming_is_trimmed_to_the_sample() {
    const ONSET: usize = 8_820;
    let pcm_onset = onset(&sine_after(ONSET, 440.0), 0.05);
    assert_eq!(pcm_onset, ONSET + 2);
    let original = std::fs::read(fixture("tone-440-44k1-onset-200ms.m4a")).unwrap();
    let directory = tempfile::tempdir().unwrap();
    assert_eq!(
        MAX_PRIMING_FRAMES, 4_096,
        "the bound the cases are built on"
    );
    let untrimmed = variant(directory.path(), &original, PAST_THE_BOUND);
    let untrimmed = channels_of(&untrimmed).remove(0);
    assert_eq!(onset(&untrimmed, 0.05), pcm_onset + PRIMING, "untrimmed");
    for (declared, trim) in DECLARATIONS {
        let path = variant(directory.path(), &original, declared);
        let source = channels_of(&path).remove(0);
        let at_source = onset(&source, 0.05);
        let decoded = SymphoniaAudioCodec::decode_path(&path, 0, AudioLane::Mixed).unwrap();
        let at_16k = onset(&decoded.samples, 0.05);
        println!(
            "{declared:?}: {} samples, onset at {at_source}; at 16 kHz {} samples, onset at {at_16k}",
            source.len(),
            decoded.len()
        );
        if let Declared::Neither(layout) = declared {
            assert_eq!(
                Priming::read(&path).map(|p| (p.ticks, p.source)),
                (layout == Layout::Recorder).then_some((2_112, PrimingSource::Unstated)),
                "{declared:?}"
            );
        }
        let expected = pcm_onset + PRIMING - trim;
        assert_eq!(at_source, expected, "{declared:?}: onset");
        let expected_16k = (expected * 16_000 + 22_050) / 44_100;
        assert!(
            at_16k.abs_diff(expected_16k) <= 1,
            "{declared:?}: onset at {at_16k} at 16 kHz, expected {expected_16k}"
        );
        // Nothing before the tone but the encoder's quiet pre-echo.
        let before = rms_decibels(&source[..expected - 1_024]);
        assert!(before < -60.0, "{declared:?}: {before} dB before the onset");
        assert!(
            same_bits(&source, &untrimmed[trim..]),
            "{declared:?}: the untrimmed samples less {trim}"
        );
    }
}

/// The same cuts on two channels, a 440 Hz sine from sample 8 820 on the
/// first and a 1 kHz one from 11 025 on the second (ffmpeg, as the mono
/// fixture): each channel's onset moves by the cut, and each channel is
/// the untrimmed one less exactly its cut, so the cut drops whole frames,
/// never one channel's samples. A stereo file in `AVAudioRecorder`'s
/// layout loses Apple's 2 112 frames as a mono one does.
#[test]
fn a_stereo_track_loses_the_same_frames_on_each_channel() {
    let pcm_onsets = [
        onset(&sine_after(8_820, 440.0), 0.05),
        onset(&sine_after(11_025, 1_000.0), 0.05),
    ];
    let original = std::fs::read(fixture("tone-440-1000-44k1-stereo-onset.m4a")).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let untrimmed = channels_of(&variant(directory.path(), &original, PAST_THE_BOUND));
    assert_eq!(untrimmed.len(), 2, "two channels");
    for (declared, trim) in DECLARATIONS {
        let path = variant(directory.path(), &original, declared);
        let channels = channels_of(&path);
        assert_eq!(channels.len(), 2, "{declared:?}: two channels");
        for (channel, samples) in channels.iter().enumerate() {
            assert_eq!(
                onset(samples, 0.05),
                pcm_onsets[channel] + PRIMING - trim,
                "{declared:?}: channel {channel}'s onset"
            );
            assert!(
                same_bits(samples, &untrimmed[channel][trim..]),
                "{declared:?}: channel {channel}, the untrimmed samples less {trim}"
            );
        }
    }
}

/// The phone's own layout: `AVAudioRecorder` writes AAC in MP4 with no
/// edit list and no gapless tag, though Apple's encoder primed 2 112
/// samples, and AVFoundation drops them. Two fixtures made on a Mac with
/// the phone's settings (AAC, 44.1 kHz mono, 64 kbps, quality 96; recipe
/// in `Tests/Fixtures/README.md`): half a second of silence from
/// `AVAudioRecorder` itself (the microphone gave zeros), and the onset
/// fixture's PCM through `AVAudioFile`, its gapless tag dropped to match
/// the recorder's layout. The decoder's lengths and onset are
/// AVFoundation's, read off the same files.
#[test]
fn the_phones_recorder_layout_decodes_as_avfoundation_reads_it() {
    let recorder = fixture("silence-44k1-avaudiorecorder.m4a");
    let apple = fixture("tone-440-44k1-onset-200ms-apple.m4a");
    for path in [&recorder, &apple] {
        assert_eq!(
            Priming::read(path).map(|p| (p.ticks, p.source)),
            Some((2_112, PrimingSource::Unstated)),
            "{}",
            path.display()
        );
    }
    // AVAudioFile: 22 528 frames in the track, 20 416 read.
    let silence = channels_of(&recorder);
    assert_eq!(silence.len(), 1);
    assert_eq!(silence[0].len(), 20_416, "AVFoundation's length");
    // AVAudioFile: 24 576 frames in the track, 22 464 read, onset at 8 823.
    let tone = channels_of(&apple).remove(0);
    assert_eq!(tone.len(), 22_464, "AVFoundation's length");
    assert_eq!(onset(&tone, 0.05), 8_823, "AVFoundation's onset");
    let decoded = SymphoniaAudioCodec::decode_path(&apple, 0, AudioLane::Mixed).unwrap();
    let at_16k = onset(&decoded.samples, 0.05);
    assert!(at_16k.abs_diff(3_201) <= 1, "onset at {at_16k} at 16 kHz");
}

fn same_bits(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.to_bits() == b.to_bits())
}

/// The phone's 44.1 kHz through the sinc resampler: level within 0.1 dB,
/// period kept, exact length.
#[test]
fn the_sinc_resampler_keeps_level_and_period_at_44100() {
    let count = 44_100 * 2;
    let output = SymphoniaAudioCodec::to_16k(&tone_44k1(1_000.0, count), 44_100);
    assert_eq!(output.len(), 32_000);
    let steady = &output[1_000..31_000];
    let error = level_against_sine(steady, 0.5);
    assert!(error.abs() < 0.1, "{error} dB");
    let hertz = frequency(steady, 16_000.0);
    assert!((hertz - 1_000.0).abs() < 5.0, "{hertz} Hz");

    // The passband edge, as the module doc states it: within 0.3 dB at
    // 6 kHz, about -1.3 dB at 6.5 kHz.
    let droop = |hertz: f64| {
        let output = SymphoniaAudioCodec::to_16k(&tone_44k1(hertz, count), 44_100);
        level_against_sine(&output[1_000..31_000], 0.5)
    };
    let at_6k = droop(6_000.0);
    let at_6k5 = droop(6_500.0);
    assert!(at_6k.abs() < 0.3, "6 kHz at {at_6k} dB");
    assert!((-1.6..=-1.0).contains(&at_6k5), "6.5 kHz at {at_6k5} dB");

    // Above the output Nyquist is rejected.
    let rejected = SincResampler::new(44_100.0, 16_000.0).resample(&tone_44k1(12_000.0, count));
    let rejection = level_against_sine(&rejected[1_000..31_000], 0.5);
    assert!(rejection < -50.0, "12 kHz aliases at {rejection} dB");
}

/// A sidecar is taken only when it is exactly as long as the master's
/// 16 kHz lane. Its 32-bit size fields wrap after 37.3 hours, so the
/// header of a long sidecar can claim a short lane and still parse; here
/// a three-second sidecar's header claims 1 000 samples and a chunk after
/// them covers the rest, as a wrapped size can land. That, a sidecar a
/// frame longer and one a frame shorter (a full disk stopped it after
/// the master's frame), and one a sample off either way all decode from
/// the master, sample for sample; the writer's own sidecar is still taken.
#[tokio::test]
async fn a_sidecar_whose_length_disagrees_with_the_master_is_not_taken() {
    let directory = tempfile::tempdir().unwrap();
    let layout = RecordingLayout::new(directory.path(), Uuid::new_v4());
    let asset = call_asset(&write_call(&layout, 3.0, true).files());
    let codec = SymphoniaAudioCodec::new();
    let master = steno_core::paths::file_url_path(&asset.url).unwrap();
    let from_master = SymphoniaAudioCodec::decode_path(&master, 0, AudioLane::Mic).unwrap();
    assert_eq!(from_master.len(), 48_000);
    let own = codec.decode(&asset, AudioLane::Mic).await.unwrap();
    assert_eq!(
        own.samples,
        WavFile::read_16k_mono(&layout.sidecar(AudioLane::Mic)).unwrap(),
        "the writer's sidecar is taken"
    );

    let samples: Vec<i16> = (0..48_000).map(|i| ((i % 200) as i16 - 100) * 50).collect();
    let mut wrapped = WavStreamWriter::header(16_000, 1_000);
    for sample in &samples {
        wrapped.extend_from_slice(&sample.to_le_bytes());
    }
    let after = WavStreamWriter::HEADER_SIZE + 2_000;
    let rest = (wrapped.len() - after - 8) as u32;
    wrapped[after..after + 4].copy_from_slice(b"junk");
    wrapped[after + 4..after + 8].copy_from_slice(&rest.to_le_bytes());
    let wrapped_path = directory.path().join("wrapped.wav");
    std::fs::write(&wrapped_path, wrapped).unwrap();
    assert_eq!(
        WavFile::read_16k_mono(&wrapped_path).unwrap().len(),
        1_000,
        "the header parses and claims 1 000 samples"
    );
    let write_sidecar = |name: &str, count: usize| {
        let path = directory.path().join(name);
        let mut writer = WavStreamWriter::create(&path, 16_000).unwrap();
        writer.write(&samples[..count.min(48_000)]).unwrap();
        writer
            .write(&vec![0i16; count.saturating_sub(48_000)])
            .unwrap();
        writer.finish().unwrap();
        path
    };
    let longer = write_sidecar("longer.wav", 48_160);
    let shorter = write_sidecar("shorter.wav", 47_840);
    let one_longer = write_sidecar("one-longer.wav", 48_001);
    let one_shorter = write_sidecar("one-shorter.wav", 47_999);
    for (name, path) in [
        ("wrapped", &wrapped_path),
        ("longer", &longer),
        ("shorter", &shorter),
        ("a sample longer", &one_longer),
        ("a sample shorter", &one_shorter),
    ] {
        let mut with = asset.clone();
        with.sidecars_16k
            .insert(AudioLane::Mic, file_url(path, false));
        let decoded = codec.decode(&with, AudioLane::Mic).await.unwrap();
        assert!(
            decoded.samples.len() == from_master.len()
                && decoded
                    .samples
                    .iter()
                    .zip(&from_master.samples)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
            "{name} sidecar: decoded from the master"
        );
    }
}

/// A master that fails after its sidecar was set aside for disagreeing
/// with it (here the asset lists a lane the master has no channel for,
/// which fails the same way an I/O error mid-file does) falls back to
/// that sidecar: a transcript from a sidecar beats none.
#[tokio::test]
async fn a_master_that_fails_falls_back_to_the_sidecar_that_disagreed() {
    let directory = tempfile::tempdir().unwrap();
    let layout = RecordingLayout::new(directory.path(), Uuid::new_v4());
    let mut asset = call_asset(&write_call(&layout, 1.0, true).files());
    let samples: Vec<i16> = (0..16_160).map(|i| ((i % 100) as i16 - 50) * 100).collect();
    let mixed = directory.path().join("mixed.wav");
    let mut writer = WavStreamWriter::create(&mixed, 16_000).unwrap();
    writer.write(&samples).unwrap();
    writer.finish().unwrap();
    asset.lanes.push(AudioLane::Mixed);
    asset
        .sidecars_16k
        .insert(AudioLane::Mixed, file_url(&mixed, false));
    let master = steno_core::paths::file_url_path(&asset.url).unwrap();
    assert!(matches!(
        SymphoniaAudioCodec::decode_path(&master, 2, AudioLane::Mixed),
        Err(CodecError::ChannelMissing { .. })
    ));

    let decoded = SymphoniaAudioCodec::new()
        .decode(&asset, AudioLane::Mixed)
        .await
        .unwrap();
    assert_eq!(decoded.samples, WavFile::read_16k_mono(&mixed).unwrap());

    std::fs::remove_file(&mixed).unwrap();
    assert!(
        SymphoniaAudioCodec::new()
            .decode(&asset, AudioLane::Mixed)
            .await
            .is_err(),
        "without the sidecar the master's failure stands"
    );
}

/// The payload of each AAC packet of an ffmpeg fixture, in file order:
/// the `stsz` sizes from the one chunk `stco` names (ffmpeg writes a
/// short file as one chunk).
fn packet_payloads(bytes: &[u8]) -> Vec<std::ops::Range<usize>> {
    let table = [b"moov", b"trak", b"mdia", b"minf", b"stbl"];
    let word = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let stco = box_offset(bytes, &[table.as_slice(), &[b"stco"]].concat());
    assert_eq!(word(stco + 12), 1, "one chunk");
    let stsz = box_offset(bytes, &[table.as_slice(), &[b"stsz"]].concat());
    assert_eq!(word(stsz + 12), 0, "a size per packet");
    let mut at = word(stco + 16);
    (0..word(stsz + 16))
        .map(|packet| {
            let size = word(stsz + 20 + 4 * packet);
            at += size;
            at - size..at
        })
        .collect()
}

/// The fixture `name` with `change` applied to its bytes, given the
/// payload of each of its packets, written into `directory` as
/// `damaged.m4a`.
fn damaged_copy(
    directory: &Path,
    name: &str,
    change: impl FnOnce(&mut [u8], &[std::ops::Range<usize>]),
) -> PathBuf {
    let mut bytes = std::fs::read(fixture(name)).unwrap();
    let payloads = packet_payloads(&bytes);
    change(&mut bytes, &payloads);
    let path = directory.join("damaged.m4a");
    std::fs::write(&path, bytes).unwrap();
    path
}

/// The clean tone fixture with the payload of `packets` zeroed, which
/// symphonia rejects as invalid data, written into `directory`.
fn with_zeroed_packets(directory: &Path, packets: impl IntoIterator<Item = usize>) -> PathBuf {
    damaged_copy(directory, "tone-440-44k1-500ms.m4a", |bytes, payloads| {
        for packet in packets {
            bytes[payloads[packet].clone()].fill(0);
        }
    })
}

/// Frames per AAC packet.
const PACKET: usize = 1_024;

/// The largest difference between `a` and `b` over `frames`.
fn largest_difference(a: &[f32], b: &[f32], frames: std::ops::Range<usize>) -> f32 {
    frames.map(|i| (a[i] - b[i]).abs()).fold(0.0, f32::max)
}

/// A packet that does not decode becomes silence of its length, in place.
/// The damaged fixture has packets 9, 10 and 15 of 23 overwritten (zeros,
/// `0xAA` and `0x55`: invalid data, a program config element and a
/// coupling channel element, the last two of which stopped the whole
/// decode before); with the 1 024 frames of priming trimmed, packet k
/// holds frames (k - 1) * 1 024 to k * 1 024. The decode is as long as the
/// clean one at the source rate and at 16 kHz, and so is the mixdown;
/// the damaged packets read as exact silence; everything before the first
/// is the clean decode bit for bit; the first packet after each damaged
/// run comes from a fresh decoder, so it is no louder than the clean
/// decode there (the stale overlap would click above it); the rest is the
/// clean decode within 2 * 10^-3, the noise the decoder substitutes in a
/// few bands coming from a generator that started over (most of it within
/// 10^-5); and the three are counted, with their 3 * 1 024 frames of
/// silence.
#[tokio::test]
async fn a_damaged_packet_becomes_silence_of_its_length() {
    let clean_path = fixture("tone-440-44k1-500ms.m4a");
    let damaged_path = fixture("tone-440-44k1-500ms-damaged.m4a");
    let clean = SymphoniaAudioCodec::read_channel(&clean_path, 0, AudioLane::Mixed).unwrap();
    let damaged = SymphoniaAudioCodec::read_channel(&damaged_path, 0, AudioLane::Mixed).unwrap();
    assert_eq!(clean.damage.parts, 0);
    assert_eq!(damaged.damage.parts, 3);
    assert!((damaged.damage.seconds - 3.0 * PACKET as f64 / 44_100.0).abs() < 1e-9);
    assert_eq!(damaged.samples.len(), clean.samples.len());
    assert_eq!(
        clean.samples.len(),
        22 * PACKET,
        "23 packets less the priming"
    );
    let (a, b) = (&damaged.samples, &clean.samples);
    let at = |packet: usize| (packet - 1) * PACKET;
    assert!(same_bits(&a[..at(9)], &b[..at(9)]), "before the damage");
    for silent in [at(9)..at(11), at(15)..at(16)] {
        assert!(a[silent.clone()].iter().all(|&s| s == 0.0), "{silent:?}");
        assert!(
            b[silent].iter().any(|&s| s.abs() > 0.1),
            "the clean decode is loud there"
        );
    }
    for after in [at(12)..at(15), at(17)..a.len()] {
        let largest = largest_difference(a, b, after.clone());
        assert!(largest < 2e-3, "{after:?}: {largest}");
    }
    // The fresh decoder: the stale tail of the packet before the damage
    // would add to the first one after it, peaking 0.17 over the tone.
    let peak = |samples: &[f32], frames: std::ops::Range<usize>| {
        samples[frames]
            .iter()
            .fold(0.0f32, |peak, s| peak.max(s.abs()))
    };
    for first in [at(11)..at(12), at(16)..at(17)] {
        let (damaged, clean) = (peak(a, first.clone()), peak(b, first.clone()));
        assert!(damaged <= clean + 0.01, "{first:?}: {damaged} over {clean}");
    }
    let quiet =
        largest_difference(a, b, at(12)..at(15)).max(largest_difference(a, b, at(17)..at(21)));
    assert!(quiet < 1e-5, "{quiet}");

    let clean_16k = SymphoniaAudioCodec::decode_path(&clean_path, 0, AudioLane::Mixed).unwrap();
    let damaged_16k = SymphoniaAudioCodec::decode_path(&damaged_path, 0, AudioLane::Mixed).unwrap();
    assert_eq!(damaged_16k.len(), clean_16k.len());
    assert_eq!((clean_16k.damage.parts, damaged_16k.damage.parts), (0, 3));
    let directory = tempfile::tempdir().unwrap();
    let asset = |path: &Path| make_asset(path, AudioFormat::M4aAac, &[AudioLane::Mixed], &[]);
    let decoded = SymphoniaAudioCodec::new()
        .decode(&asset(&damaged_path), AudioLane::Mixed)
        .await
        .unwrap();
    assert_eq!(decoded, damaged_16k, "the decoder reports the count");
    for (name, path) in [("clean", &clean_path), ("damaged", &damaged_path)] {
        let to = directory.path().join(format!("{name}.wav"));
        SymphoniaAudioCodec::new()
            .mixdown(&asset(path), &to)
            .await
            .unwrap();
        assert_eq!(
            WavFile::read_16k_mono(&to).unwrap().len(),
            clean_16k.len(),
            "{name}"
        );
    }
}

/// A damaged packet inside the encoder priming is trimmed with it: packet
/// 0 is all priming (1 024 frames), so its silence is cut whole, the decode
/// keeps its length and no second of it is silent, and from the second
/// packet after it is the clean decode within the noise generator's drift.
#[test]
fn a_damaged_packet_in_the_priming_is_trimmed_with_it() {
    let directory = tempfile::tempdir().unwrap();
    let clean =
        SymphoniaAudioCodec::read_channel(&fixture("tone-440-44k1-500ms.m4a"), 0, AudioLane::Mixed)
            .unwrap();
    let path = with_zeroed_packets(directory.path(), [0]);
    let damaged = SymphoniaAudioCodec::read_channel(&path, 0, AudioLane::Mixed).unwrap();
    assert_eq!(damaged.damage.parts, 1);
    assert_eq!(damaged.damage.seconds, 0.0, "the priming took all of it");
    assert_eq!(damaged.samples.len(), clean.samples.len());
    let largest = largest_difference(
        &damaged.samples,
        &clean.samples,
        PACKET..clean.samples.len(),
    );
    assert!(largest < 2e-3, "{largest}");
}

/// A damaged run from the first packet, before any packet gave the
/// stream's shape (symphonia's MP4 reader declares no channel count for
/// AAC), is held until the first packet that decodes, then becomes
/// silence of its length less the priming: packets 0 to 3 zeroed are
/// three packets of silence after the 1 024 frames of priming, the decode
/// keeps the clean length at the source rate and at 16 kHz, and the audio
/// after it sits where the clean decode has it.
#[test]
fn a_damaged_run_from_the_first_packet_keeps_its_length() {
    let directory = tempfile::tempdir().unwrap();
    let clean_path = fixture("tone-440-44k1-500ms.m4a");
    let clean = SymphoniaAudioCodec::read_channel(&clean_path, 0, AudioLane::Mixed).unwrap();
    let path = with_zeroed_packets(directory.path(), 0..4);
    let damaged = SymphoniaAudioCodec::read_channel(&path, 0, AudioLane::Mixed).unwrap();
    assert_eq!(damaged.damage.parts, 4);
    let silent = 3 * PACKET;
    assert!((damaged.damage.seconds - silent as f64 / 44_100.0).abs() < 1e-9);
    assert_eq!(damaged.samples.len(), clean.samples.len());
    assert!(damaged.samples[..silent].iter().all(|&s| s == 0.0));
    let after = silent + PACKET..clean.samples.len();
    let largest = largest_difference(&damaged.samples, &clean.samples, after.clone());
    assert!(largest < 2e-3, "{after:?}: {largest}");
    assert_eq!(
        SymphoniaAudioCodec::decode_path(&path, 0, AudioLane::Mixed)
            .unwrap()
            .len(),
        SymphoniaAudioCodec::decode_path(&clean_path, 0, AudioLane::Mixed)
            .unwrap()
            .len()
    );
}

/// A corrupt first packet that reads as a channel pair in a mono file
/// (`0x2F` leads with a channel pair element) costs that packet only:
/// symphonia's AAC decoder fixes its channel layout at its first packet
/// before checking it, and a reset keeps that layout, so every packet
/// after it failed; the decoder after a damaged packet is a fresh one.
#[test]
fn a_first_packet_read_as_a_channel_pair_costs_only_itself() {
    let directory = tempfile::tempdir().unwrap();
    let clean =
        SymphoniaAudioCodec::read_channel(&fixture("tone-440-44k1-500ms.m4a"), 0, AudioLane::Mixed)
            .unwrap();
    let path = damaged_copy(
        directory.path(),
        "tone-440-44k1-500ms.m4a",
        |bytes, payloads| {
            bytes[payloads[0].clone()].fill(0x2F);
        },
    );
    let damaged = SymphoniaAudioCodec::read_channel(&path, 0, AudioLane::Mixed).unwrap();
    assert_eq!(damaged.damage.parts, 1);
    assert_eq!(damaged.samples.len(), clean.samples.len());
}

/// A packet that makes symphonia's AAC decoder panic (an index out of
/// bounds in its section data, reached from a fresh decoder) is a damaged
/// packet too: the panic is caught, the packet becomes silence, the
/// decoder is replaced, and the file decodes. In the stereo fixture,
/// packet 21 with its bytes from 27 on set to `0xFF` fails, and packet 22,
/// zeros but for a 4 at byte 6, then panics the fresh decoder. Packet 22
/// is the last, whose container duration is 546 frames where its decode
/// gives 1 024, so the decode is 478 frames shorter than the clean one.
#[test]
fn a_packet_that_panics_the_decoder_becomes_silence() {
    thread_local! {
        static PANICS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        PANICS.with(|count| count.set(count.get() + 1));
        previous(panic);
    }));
    let directory = tempfile::tempdir().unwrap();
    let name = "tone-440-1000-44k1-stereo-onset.m4a";
    let clean = SymphoniaAudioCodec::read_channel(&fixture(name), 1, AudioLane::Mixed).unwrap();
    let path = damaged_copy(directory.path(), name, |bytes, payloads| {
        let (failing, panicking) = (payloads[21].clone(), payloads[22].clone());
        bytes[failing.start + 27..failing.end].fill(0xFF);
        bytes[panicking.clone()].fill(0);
        bytes[panicking.start + 6] = 4;
    });
    let damaged = SymphoniaAudioCodec::read_channel(&path, 1, AudioLane::Mixed).unwrap();
    assert_eq!(PANICS.with(std::cell::Cell::get), 1, "the decoder panicked");
    assert_eq!(damaged.damage.parts, 2);
    assert_eq!(damaged.samples.len(), clean.samples.len() - (PACKET - 546));
    let at = |packet: usize| (packet - 1) * PACKET;
    assert!(same_bits(
        &damaged.samples[..at(21)],
        &clean.samples[..at(21)]
    ));
    assert!(damaged.samples[at(21)..].iter().all(|&s| s == 0.0));
}

/// A stereo file's damaged packets are silence in both channels, of the
/// packet's length in frames: packets 12, 13 and 18 of the stereo fixture
/// zeroed, after both of its tones began. Each channel keeps its clean length, at the source rate, at
/// 16 kHz and in the mixdown, the damaged packets are exact zeros,
/// everything before them is the clean decode bit for bit, and the rest is
/// within 5 * 10^-3 of it (the noise generator's drift, larger in the
/// second channel's 1 kHz tone than in the mono fixture).
#[tokio::test]
async fn a_stereo_files_damaged_packets_are_silence_in_both_channels() {
    let directory = tempfile::tempdir().unwrap();
    let name = "tone-440-1000-44k1-stereo-onset.m4a";
    let path = damaged_copy(directory.path(), name, |bytes, payloads| {
        for packet in [12, 13, 18] {
            bytes[payloads[packet].clone()].fill(0);
        }
    });
    let at = |packet: usize| (packet - 1) * PACKET;
    for channel in 0..2 {
        let clean =
            SymphoniaAudioCodec::read_channel(&fixture(name), channel, AudioLane::Mixed).unwrap();
        let damaged = SymphoniaAudioCodec::read_channel(&path, channel, AudioLane::Mixed).unwrap();
        assert_eq!(damaged.damage.parts, 3);
        assert_eq!(damaged.samples.len(), clean.samples.len(), "{channel}");
        let (a, b) = (&damaged.samples, &clean.samples);
        assert!(same_bits(&a[..at(12)], &b[..at(12)]), "{channel}");
        for silent in [at(12)..at(14), at(18)..at(19)] {
            assert!(a[silent.clone()].iter().all(|&s| s == 0.0), "{silent:?}");
            assert!(b[silent].iter().any(|&s| s.abs() > 0.1), "loud there");
        }
        for after in [at(15)..at(18), at(20)..a.len()] {
            let largest = largest_difference(a, b, after.clone());
            assert!(largest < 5e-3, "{channel} {after:?}: {largest}");
        }
        assert_eq!(
            SymphoniaAudioCodec::decode_path(&path, channel, AudioLane::Mixed)
                .unwrap()
                .len(),
            SymphoniaAudioCodec::decode_path(&fixture(name), channel, AudioLane::Mixed)
                .unwrap()
                .len()
        );
    }
    let lengths: Vec<usize> = [fixture(name), path]
        .iter()
        .map(|source| {
            let to = directory.path().join("audio.wav");
            SymphoniaAudioCodec::mixdown_path(source, &to).unwrap();
            WavFile::read_16k_mono(&to).unwrap().len()
        })
        .collect();
    assert_eq!(lengths[0], lengths[1]);
}

/// The mono fixture with its media timescale doubled (88 200, twice the
/// sample rate): the `mdhd` timescale and duration, every `stts` delta and
/// the edit list's start, in place. A damaged packet's duration is then
/// 2 048 ticks, still 1 024 frames.
fn at_double_timescale(bytes: &mut [u8]) {
    let word = |bytes: &[u8], at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    let track = [b"moov", b"trak"];
    let mdhd = box_offset(bytes, &[track.as_slice(), &[b"mdia", b"mdhd"]].concat());
    let elst = box_offset(bytes, &[track.as_slice(), &[b"edts", b"elst"]].concat());
    let stts = box_offset(
        bytes,
        &[track.as_slice(), &[b"mdia", b"minf", b"stbl", b"stts"]].concat(),
    );
    assert_eq!(
        (bytes[mdhd + 8], bytes[elst + 8]),
        (0, 0),
        "version 0 boxes"
    );
    let entries = word(bytes, stts + 12) as usize;
    let deltas = (0..entries).map(|entry| stts + 20 + 8 * entry);
    for at in [mdhd + 20, mdhd + 24, elst + 20].into_iter().chain(deltas) {
        let doubled = word(bytes, at) * 2;
        bytes[at..at + 4].copy_from_slice(&doubled.to_be_bytes());
    }
}

/// A damaged packet's length goes through the track's time base: in a
/// file whose timescale is twice its rate, the clean decode is the
/// original's bit for bit, and packets 9, 10 and 15 zeroed are 1 024
/// frames of silence each, so the decode keeps the clean length.
#[test]
fn a_damaged_packets_length_goes_through_the_time_base() {
    let directory = tempfile::tempdir().unwrap();
    let name = "tone-440-44k1-500ms.m4a";
    let clean = SymphoniaAudioCodec::read_channel(&fixture(name), 0, AudioLane::Mixed).unwrap();
    let rescaled = damaged_copy(directory.path(), name, |bytes, _| {
        at_double_timescale(bytes);
    });
    let rescaled = SymphoniaAudioCodec::read_channel(&rescaled, 0, AudioLane::Mixed).unwrap();
    assert!(same_bits(&rescaled.samples, &clean.samples));
    let path = damaged_copy(directory.path(), name, |bytes, payloads| {
        at_double_timescale(bytes);
        for packet in [9, 10, 15] {
            bytes[payloads[packet].clone()].fill(0);
        }
    });
    let damaged = SymphoniaAudioCodec::read_channel(&path, 0, AudioLane::Mixed).unwrap();
    assert_eq!(damaged.damage.parts, 3);
    assert!((damaged.damage.seconds - 3.0 * PACKET as f64 / 44_100.0).abs() < 1e-9);
    assert_eq!(damaged.samples.len(), clean.samples.len());
    let at = |packet: usize| (packet - 1) * PACKET;
    assert!(damaged.samples[at(9)..at(11)].iter().all(|&s| s == 0.0));
}

/// A file with more damaged packets than clean ones fails rather than
/// decode to more silence than sound (`MAX_DAMAGED_SHARE`, a half): 11 of
/// the 23 packets damaged decode, with the count; 12, and every one, fail,
/// in the lane, at the source rate and in the mixdown, which leaves no
/// file.
#[tokio::test]
async fn a_file_with_more_damaged_packets_than_clean_ones_fails() {
    assert_eq!(steno_audio::codec::MAX_DAMAGED_SHARE, 0.5);
    let directory = tempfile::tempdir().unwrap();
    let path = with_zeroed_packets(directory.path(), 1..12);
    let decoded = SymphoniaAudioCodec::decode_path(&path, 0, AudioLane::Mixed).unwrap();
    assert_eq!(decoded.damage.parts, 11);
    for packets in [1..13, 0..23] {
        let path = with_zeroed_packets(directory.path(), packets.clone());
        for error in [
            SymphoniaAudioCodec::decode_path(&path, 0, AudioLane::Mixed).map(drop),
            SymphoniaAudioCodec::read_channel(&path, 0, AudioLane::Mixed).map(drop),
        ] {
            let error = error.unwrap_err();
            let damaged = packets.len();
            assert!(
                matches!(&error, CodecError::ConversionFailed(text) if text.ends_with(&format!("{damaged} of 23 packets do not decode"))),
                "{packets:?}: {error:?}"
            );
        }
        let to = directory.path().join("audio.wav");
        assert!(SymphoniaAudioCodec::mixdown_path(&path, &to).is_err());
        assert!(!to.exists(), "{packets:?}: the failed mixdown is removed");
    }
}
