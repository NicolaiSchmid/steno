//! The recording writer, the CAF and WAV streams, the sidecar resampler
//! and the level meter.
//! Swift: `Tests/StenoAudioTests/RecordingWriterTests.swift`,
//! `Resampler48kTo16kTests.swift`, `LevelMeterTests.swift`.

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

use std::path::Path;

use steno_audio::capture::{CaptureError, LaneLevel, LaneLevels};
use steno_audio::realtime::{LevelMeter, LevelSlot};
use steno_audio::testing::AudioFixtures;
use steno_audio::writer::{
    CafFile, CafReadError, CafStreamWriter, LaneFrames, RecordingWriter, RecordingWriting,
    Resampler48kTo16k, WavFile, WavReadError, WavStreamWriter,
};
use steno_core::{AudioFormat, AudioLane, RecordingLayout};
use uuid::Uuid;

use common::{frequency, level_against_sine};

fn write_lanes(writer: &mut RecordingWriter, lanes: &[&[f32]]) {
    let frames = lanes[0].len() / 480;
    for frame in 0..frames {
        let slices: Vec<&[f32]> = lanes
            .iter()
            .map(|lane| &lane[frame * 480..(frame + 1) * 480])
            .collect();
        writer
            .write(&LaneFrames {
                frame_count: 480,
                lanes: &slices,
                raw_mic: None,
            })
            .unwrap();
    }
}

#[test]
fn two_lanes_round_trip_sample_accurately_with_sidecars() {
    let directory = tempfile::tempdir().unwrap();
    let meeting_id = Uuid::new_v4();
    let layout = RecordingLayout::new(directory.path(), meeting_id);
    let mut writer =
        RecordingWriter::new(&layout, &[AudioLane::Mic, AudioLane::System], false).unwrap();
    let mic = AudioFixtures::tone(440.0, 2.0, 0.5);
    let system = AudioFixtures::tone(1_000.0, 2.0, 0.25);
    write_lanes(&mut writer, &[&mic, &system]);
    let files = writer.finish().unwrap();
    assert_eq!(files.master, layout.master(AudioFormat::Caf48kFloat32));
    assert_eq!(
        files.sidecars_16k[&AudioLane::Mic],
        layout.sidecar(AudioLane::Mic)
    );
    assert_eq!(
        files.sidecars_16k[&AudioLane::System],
        layout.sidecar(AudioLane::System)
    );
    assert_eq!(files.raw_mic, None);
    assert_eq!(files.duration, 2.0);

    let master = CafFile::read(&files.master).unwrap();
    assert_eq!(master.sample_rate, 48_000.0);
    assert_eq!(master.channels.len(), 2);
    assert_eq!(master.frame_count(), 96_000);
    assert_eq!(
        master.channels[0], mic,
        "channel 0 is the mic lane, bit for bit"
    );
    assert_eq!(master.channels[1], system);

    let mic_sidecar = WavFile::read_16k_mono(&files.sidecars_16k[&AudioLane::Mic]).unwrap();
    let system_sidecar = WavFile::read_16k_mono(&files.sidecars_16k[&AudioLane::System]).unwrap();
    assert_eq!(mic_sidecar.len(), 32_000);
    assert_eq!(system_sidecar.len(), 32_000);
    assert!(level_against_sine(&mic_sidecar[2_000..], 0.5).abs() < 0.1);
    assert!(level_against_sine(&system_sidecar[2_000..], 0.25).abs() < 0.1);
    let info = WavFile::read(&files.sidecars_16k[&AudioLane::Mic]).unwrap();
    assert_eq!(
        (info.sample_rate, info.channels.len(), info.bits_per_sample),
        (16_000, 1, 16)
    );
}

#[test]
fn raw_mic_lane_is_written_beside_the_master() {
    let directory = tempfile::tempdir().unwrap();
    let layout = RecordingLayout::new(directory.path(), Uuid::new_v4());
    let mut writer =
        RecordingWriter::new(&layout, &[AudioLane::Mic, AudioLane::System], true).unwrap();
    let processed = [0.1f32; 480];
    let raw = [0.4f32; 480];
    let system = [0.0f32; 480];
    for _ in 0..10 {
        writer
            .write(&LaneFrames {
                frame_count: 480,
                lanes: &[&processed, &system],
                raw_mic: Some(&raw),
            })
            .unwrap();
    }
    let files = writer.finish().unwrap();
    let raw_path = files.raw_mic.unwrap();
    assert_eq!(raw_path.file_name().unwrap(), "mic.raw.caf");
    let raw_file = CafFile::read(&raw_path).unwrap();
    assert_eq!(raw_file.channels.len(), 1);
    assert_eq!(raw_file.frame_count(), 4_800);
    assert!(raw_file.channels[0].iter().all(|s| *s == 0.4));
    assert!(
        CafFile::read(&files.master).unwrap().channels[0]
            .iter()
            .all(|s| *s == 0.1)
    );
}

#[test]
fn in_person_writes_one_channel_and_the_mixed_sidecar() {
    let directory = tempfile::tempdir().unwrap();
    let layout = RecordingLayout::new(directory.path(), Uuid::new_v4());
    let mut writer = RecordingWriter::new(&layout, &[AudioLane::Mixed], false).unwrap();
    let frame = AudioFixtures::tone(500.0, 0.5, 0.5);
    write_lanes(&mut writer, &[&frame]);
    let files = writer.finish().unwrap();
    assert_eq!(
        files.sidecars_16k.keys().copied().collect::<Vec<_>>(),
        vec![AudioLane::Mixed]
    );
    assert_eq!(
        files.sidecars_16k[&AudioLane::Mixed].file_name().unwrap(),
        "mixed.wav"
    );
    assert_eq!(CafFile::read(&files.master).unwrap().channels.len(), 1);
    assert_eq!(files.duration, 0.5);
}

#[test]
fn wrong_frame_shape_and_double_finish_fail() {
    let directory = tempfile::tempdir().unwrap();
    let mut writer = RecordingWriter::new(
        &RecordingLayout::from_directory(directory.path()),
        &[AudioLane::Mixed],
        false,
    )
    .unwrap();
    let short = [0.0f32; 100];
    assert!(matches!(
        writer.write(&LaneFrames {
            frame_count: 100,
            lanes: &[&short],
            raw_mic: None
        }),
        Err(CaptureError::WriterFailed(_))
    ));
    writer.finish().unwrap();
    assert!(matches!(
        writer.finish(),
        Err(CaptureError::WriterFailed(_))
    ));
}

/// The master is readable before `finish()`: the data chunk says -1 and the
/// reader takes everything to the end of the file.
#[test]
fn unfinished_master_is_readable_to_the_last_frame() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("streaming.caf");
    let mut writer = CafStreamWriter::create(&path, 48_000.0, 2).unwrap();
    let frame = [0.25f32; 960];
    writer.write(&frame, 480).unwrap();
    writer.write(&frame, 480).unwrap();
    let partial = CafFile::read(&path).unwrap();
    assert_eq!(partial.frame_count(), 960);
    assert_eq!(partial.channels[1][959], 0.25);
    writer.finish().unwrap();
    let finished = CafFile::read(&path).unwrap();
    assert_eq!(finished.frame_count(), 960);
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(
        bytes.len(),
        CafStreamWriter::HEADER_SIZE + 960 * 8,
        "the header, then the samples"
    );
    let size = bytes[56..64]
        .iter()
        .fold(0i64, |acc, b| acc << 8 | i64::from(*b));
    assert_eq!(size, 4 + 960 * 8);
}

/// A process killed inside a write leaves a partial trailing frame; the
/// reader takes whole frames and ignores the tail.
#[test]
fn a_truncated_master_reads_whole_frames_only() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("killed.caf");
    let mut writer = CafStreamWriter::create(&path, 48_000.0, 2).unwrap();
    let frame: Vec<f32> = (0..960).map(|i| i as f32).collect();
    writer.write(&frame, 480).unwrap();
    let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len((68 + 479 * 8 + 5) as u64).unwrap();
    drop(file);
    let partial = CafFile::read(&path).unwrap();
    assert_eq!(partial.frame_count(), 479);
    assert_eq!(partial.channels[0].last(), Some(&956.0));
    assert_eq!(partial.channels[1].last(), Some(&957.0));
}

/// The sidecar is the master's lane at a third of the rate: an onset at
/// 1.0 s on the mic lane lands at 16 000 samples in `mic.wav` plus the
/// resampler's 32-sample group delay, exact zeros before it, and nothing at
/// all in `system.wav`.
#[test]
fn sidecars_align_with_the_master_and_their_lane() {
    let directory = tempfile::tempdir().unwrap();
    let layout = RecordingLayout::new(directory.path(), Uuid::new_v4());
    let mut writer =
        RecordingWriter::new(&layout, &[AudioLane::Mic, AudioLane::System], false).unwrap();
    let mut mic = vec![0.0f32; 96_000];
    mic[48_000..].copy_from_slice(&AudioFixtures::tone(1_000.0, 1.0, 0.5));
    let system = vec![0.0f32; 96_000];
    write_lanes(&mut writer, &[&mic, &system]);
    let files = writer.finish().unwrap();

    let master = CafFile::read(&files.master).unwrap();
    assert!(master.channels[0][..48_000].iter().all(|s| *s == 0.0));
    assert_ne!(master.channels[0][48_001], 0.0);
    assert!(master.channels[1].iter().all(|s| *s == 0.0));

    let mic_sidecar = WavFile::read_16k_mono(&files.sidecars_16k[&AudioLane::Mic]).unwrap();
    let system_sidecar = WavFile::read_16k_mono(&files.sidecars_16k[&AudioLane::System]).unwrap();
    assert_eq!(mic_sidecar.len(), 32_000);
    assert!(
        mic_sidecar[..16_000].iter().all(|s| *s == 0.0),
        "causal: nothing before the onset"
    );
    let onset = mic_sidecar.iter().position(|s| s.abs() > 0.1).unwrap();
    assert!((16_020..=16_050).contains(&onset), "onset at {onset}");
    assert!(
        system_sidecar.iter().all(|s| *s == 0.0),
        "the silent lane's sidecar stays silent"
    );
}

/// The RIFF size field counts everything after itself (36 header bytes plus
/// the samples) and the `data` size the samples alone; both are checked
/// byte for byte, as is the file length, since the reader tolerates an
/// overstated RIFF size and would not notice one.
#[test]
fn wav_header_sizes_match_the_samples_written() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sizes.wav");
    let mut writer = WavStreamWriter::create(&path, 16_000).unwrap();
    writer.write(&[1_000i16; 160]).unwrap();
    writer.write(&[-1_000i16; 100]).unwrap();
    writer.finish().unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let field = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let data_size = 260 * 2;
    assert_eq!(bytes.len(), WavStreamWriter::HEADER_SIZE + data_size);
    assert_eq!(field(4), 36 + data_size, "RIFF size");
    assert_eq!(field(4), bytes.len() - 8);
    assert_eq!(&bytes[36..40], b"data");
    assert_eq!(field(40), data_size, "data size");
    assert_eq!(field(24), 16_000, "sample rate");
}

/// A sidecar whose writer never reached `finish()` keeps its zero-size
/// header with the samples after it, so the RIFF parser rejects it as
/// malformed; the decoder relies on exactly that to rebuild the lane from
/// the master. Finishing repairs it.
#[test]
fn an_unfinished_sidecar_is_rejected_until_finished() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mic.wav");
    let mut writer = WavStreamWriter::create(&path, 16_000).unwrap();
    writer.write(&[1_000i16; 160]).unwrap();
    assert_eq!(
        std::fs::read(&path).unwrap().len(),
        WavStreamWriter::HEADER_SIZE + 320
    );
    assert!(matches!(
        WavFile::read(&path),
        Err(WavReadError::Malformed(_))
    ));
    writer.finish().unwrap();
    assert_eq!(WavFile::read_16k_mono(&path).unwrap().len(), 160);
}

#[test]
fn caf_reader_rejects_garbage() {
    assert!(matches!(
        CafFile::read_bytes(&[0x41; 64]),
        Err(CafReadError::Malformed(_))
    ));
    assert!(matches!(
        CafFile::read_bytes(b"caff"),
        Err(CafReadError::Malformed(_))
    ));
    assert!(matches!(
        CafFile::read(Path::new("/nonexistent/x.caf")),
        Err(CafReadError::Io(_))
    ));
}

// Resampler

fn resample_int16(input: &[f32], reset_at_frame: Option<usize>) -> Vec<i16> {
    let mut resampler = Resampler48kTo16k::new(480);
    let mut output = Vec::new();
    let mut frame = [0i16; 160];
    for (index, chunk) in input.as_chunks::<480>().0.iter().enumerate() {
        if Some(index) == reset_at_frame {
            resampler.reset();
        }
        resampler.process(chunk, &mut frame);
        output.extend_from_slice(&frame);
    }
    output
}

fn resample(input: &[f32]) -> Vec<f32> {
    resample_int16(input, None)
        .iter()
        .map(|s| f32::from(*s) / 32767.0)
        .collect()
}

#[test]
fn one_kilohertz_keeps_its_level_and_period() {
    let output = resample(&AudioFixtures::tone(1_000.0, 1.0, 0.5));
    assert_eq!(output.len(), 16_000);
    let steady = &output[2_000..];
    let error = level_against_sine(steady, 0.5);
    assert!(error.abs() < 0.1, "{error} dB");
    assert!((frequency(steady, 16_000.0) - 1_000.0).abs() < 5.0);
    assert!(steady.iter().copied().fold(f32::MIN, f32::max) <= 0.505);
}

#[test]
fn passband_edge_survives_and_stopband_is_rejected() {
    let level = |hertz: f64| {
        level_against_sine(
            &resample(&AudioFixtures::tone(hertz, 1.0, 0.5))[2_000..],
            0.5,
        )
    };
    let six_error = level(6_000.0);
    assert!(six_error.abs() < 0.5, "6 kHz: {six_error} dB");
    let rejection = level(12_000.0);
    assert!(rejection < -60.0, "12 kHz aliases at {rejection} dB");
    let nine_level = level(9_000.0);
    assert!(nine_level < -40.0, "9 kHz aliases at {nine_level} dB");
}

/// Processed 480 samples at a time, the output is the input low-passed and
/// delayed by exactly half the filter, with no seam at any frame boundary.
#[test]
fn output_follows_the_input_with_a_fixed_delay_across_frame_boundaries() {
    let output = resample(&AudioFixtures::tone(1_000.0, 1.0, 0.5));
    let delay = (Resampler48kTo16k::TAPS - 1) as f64 / 2.0 / Resampler48kTo16k::FACTOR as f64;
    let mut max_error = 0.0f32;
    for (index, sample) in output.iter().enumerate().skip(500) {
        let ideal = 0.5
            * (2.0 * std::f64::consts::PI * 1_000.0 * (index as f64 - delay) / 16_000.0).sin()
                as f32;
        max_error = max_error.max((sample - ideal).abs());
    }
    assert!(
        max_error < 0.01,
        "largest deviation from the delayed sine: {max_error}"
    );
}

#[test]
fn output_clamps_to_full_scale_and_history_resets() {
    let ints = resample_int16(&AudioFixtures::tone(440.0, 0.1, 1.2), None);
    assert_eq!(ints.len(), 1_600);
    assert_eq!(ints.iter().max(), Some(&32767));
    assert_eq!(ints.iter().min(), Some(&-32767));

    let input = AudioFixtures::tone(1_000.0, 0.05, 0.5);
    let whole = resample_int16(&input, None);
    let interrupted = resample_int16(&input, Some(2));
    assert_eq!(interrupted[..320], whole[..320]);
    assert_ne!(interrupted[320..480], whole[320..480]);

    let taps = Resampler48kTo16k::kaiser_sinc(192, 7_300.0 / 48_000.0, 9.0);
    assert!((taps.iter().sum::<f32>() - 1.0).abs() < 1e-5);
    for index in 0..96 {
        assert!((taps[index] - taps[191 - index]).abs() < 1e-7);
    }
}

// Level meter

#[test]
fn the_meter_reports_rms_and_peak_in_dbfs() {
    let mut meter = LevelMeter::new();
    meter.accumulate(&AudioFixtures::tone(1_000.0, 1.0, 1.0));
    let level = meter.flush();
    assert!((level.rms - -3.01).abs() < 0.05);
    assert!(level.peak.abs() < 0.01);
    assert_eq!(
        meter.current(),
        LaneLevel::SILENCE,
        "flush resets the window"
    );

    let mut quiet = LevelMeter::new();
    quiet.accumulate(&AudioFixtures::tone(1_000.0, 1.0, 0.1));
    assert!((quiet.current().rms - -23.01).abs() < 0.05);
    assert!((quiet.current().peak - -20.0).abs() < 0.01);
    assert!((quiet.linear_peak() - 0.1).abs() < 0.001);

    let mut silent = LevelMeter::new();
    silent.accumulate(&[0.0; 480]);
    assert_eq!(silent.flush(), LaneLevel::SILENCE);
    assert_eq!(LevelMeter::decibels(0.0), -160.0);
    assert_eq!(LevelMeter::decibels(1.0), 0.0);
    assert!((LevelMeter::decibels(0.5) - -6.02).abs() < 0.01);
    assert!((LevelMeter::decibels(LaneLevel::SILENT_PEAK_LINEAR) - -80.0).abs() < 0.01);

    let mut windows = LevelMeter::new();
    windows.accumulate(&[1.0; 100]);
    windows.accumulate(&[0.0; 300]);
    assert!((windows.current().rms - -6.02).abs() < 0.01);
    assert_eq!(windows.current().peak, 0.0);
}

#[test]
fn level_slot_publishes_generations() {
    let slot = LevelSlot::new(true);
    assert_eq!(slot.current_generation(), 0);
    assert_eq!(
        slot.levels(),
        LaneLevels {
            mic: LaneLevel::SILENCE,
            system: Some(LaneLevel::SILENCE)
        }
    );
    slot.publish(
        LaneLevel {
            rms: -10.0,
            peak: -3.0,
        },
        Some(LaneLevel {
            rms: -20.0,
            peak: -12.0,
        }),
    );
    assert_eq!(slot.current_generation(), 1);
    assert_eq!(
        slot.levels(),
        LaneLevels {
            mic: LaneLevel {
                rms: -10.0,
                peak: -3.0
            },
            system: Some(LaneLevel {
                rms: -20.0,
                peak: -12.0
            })
        }
    );
    let mono = LevelSlot::new(false);
    mono.publish(
        LaneLevel {
            rms: -1.0,
            peak: 0.0,
        },
        None,
    );
    assert_eq!(mono.levels().system, None);
}
