//! Decode and mixdown on files built in setup: a two-channel 48 kHz CAF
//! from the recording writer, a 16 kHz WAV master, and the sinc resampler
//! on a 44.1 kHz tone. No m4a: there is no AAC encoder in pure Rust to
//! build one with; the AAC path is exercised by hand with a phone recording
//! (see the module doc of `steno_audio::codec`).
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

use std::collections::BTreeMap;
use std::path::Path;

use steno_audio::EchoMetrics;
use steno_audio::codec::sinc::SincResampler;
use steno_audio::codec::{CodecError, SymphoniaAudioCodec};
use steno_audio::testing::AudioFixtures;
use steno_audio::writer::{
    LaneFrames, RecordingWriter, RecordingWriting, WavFile, WavStreamWriter,
};
use steno_core::paths::file_url;
use steno_core::{
    AudioAsset, AudioDecoder, AudioFormat, AudioLane, AudioRetention, RecordingLayout,
};
use uuid::Uuid;

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

/// A two-lane meeting folder: 1 kHz on the mic lane at 0.5, 1 kHz on the
/// system lane at 0.25, two seconds, with sidecars.
fn make_call_asset(directory: &Path) -> AudioAsset {
    let layout = RecordingLayout::new(directory, Uuid::new_v4());
    let files = write_call(&layout, 2.0, true).files();
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
    let mic_difference = 20.0
        * (EchoMetrics::rms(&mic_from_master.samples[window.clone()])
            / EchoMetrics::rms(&mic_from_sidecar.samples[window.clone()]))
        .log10();
    let system_difference = 20.0
        * (EchoMetrics::rms(&system_from_master.samples[window.clone()])
            / EchoMetrics::rms(&system_from_sidecar.samples[window.clone()]))
        .log10();
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
    let files = write_call_lanes(&layout, &mic, &system, true).files();
    let sidecars: Vec<(AudioLane, &Path)> = files
        .sidecars_16k
        .iter()
        .map(|(lane, path)| (*lane, path.as_path()))
        .collect();
    let asset = make_asset(
        &files.master,
        AudioFormat::Caf48kFloat32,
        &[AudioLane::Mic, AudioLane::System],
        &sidecars,
    );
    let mut without_sidecars = asset.clone();
    without_sidecars.sidecars_16k = BTreeMap::new();
    let codec = SymphoniaAudioCodec::new();
    let from_sidecar = codec.decode(&asset, AudioLane::Mic).await.unwrap();
    let from_master = codec
        .decode(&without_sidecars, AudioLane::Mic)
        .await
        .unwrap();
    let onset = |samples: &[f32]| samples.iter().position(|s| s.abs() > 0.1).unwrap();
    let sidecar_onset = onset(&from_sidecar.samples);
    let master_onset = onset(&from_master.samples);
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
    let level = EchoMetrics::rms(&file.channels[0][8_000..28_000]);
    assert!((20.0 * (level / (0.375 / 2f32.sqrt())).log10()).abs() < 1.0);

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

/// The phone's 44.1 kHz through the sinc resampler: level within 0.1 dB,
/// period kept, exact length.
#[test]
fn the_sinc_resampler_keeps_level_and_period_at_44100() {
    let count = 44_100 * 2;
    let tone: Vec<f32> = (0..count)
        .map(|i| 0.5 * (2.0 * std::f64::consts::PI * 1_000.0 * i as f64 / 44_100.0).sin() as f32)
        .collect();
    let output = SymphoniaAudioCodec::to_16k(&tone, 44_100);
    assert_eq!(output.len(), 32_000);
    let steady = &output[1_000..31_000];
    let error = 20.0 * (EchoMetrics::rms(steady) / (0.5 / 2f32.sqrt())).log10();
    assert!(error.abs() < 0.1, "{error} dB");
    let crossings = steady
        .windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count();
    let seconds = steady.len() as f64 / 16_000.0;
    assert!(
        (crossings as f64 / seconds - 1_000.0).abs() < 5.0,
        "{crossings} crossings"
    );

    // The passband edge, as the module doc states it: within 0.3 dB at
    // 6 kHz, about -1.3 dB at 6.5 kHz.
    let droop = |frequency: f64| {
        let tone: Vec<f32> = (0..count)
            .map(|i| {
                0.5 * (2.0 * std::f64::consts::PI * frequency * i as f64 / 44_100.0).sin() as f32
            })
            .collect();
        let output = SymphoniaAudioCodec::to_16k(&tone, 44_100);
        20.0 * (EchoMetrics::rms(&output[1_000..31_000]) / (0.5 / 2f32.sqrt())).log10()
    };
    let at_6k = droop(6_000.0);
    let at_6k5 = droop(6_500.0);
    assert!(at_6k.abs() < 0.3, "6 kHz at {at_6k} dB");
    assert!((-1.6..=-1.0).contains(&at_6k5), "6.5 kHz at {at_6k5} dB");

    // Above the output Nyquist is rejected.
    let high: Vec<f32> = (0..count)
        .map(|i| 0.5 * (2.0 * std::f64::consts::PI * 12_000.0 * i as f64 / 44_100.0).sin() as f32)
        .collect();
    let rejected = SincResampler::new(44_100.0, 16_000.0).resample(&high);
    let rejection =
        20.0 * (EchoMetrics::rms(&rejected[1_000..31_000]) / (0.5 / 2f32.sqrt())).log10();
    assert!(rejection < -50.0, "12 kHz aliases at {rejection} dB");
}
