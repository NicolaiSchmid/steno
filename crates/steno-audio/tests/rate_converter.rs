//! The streaming converter from a device's rate to 48 kHz
//! (`.plans/2026-10-05-device-sample-rate.md`): chunking changes nothing,
//! it agrees with the offline sinc resampler at whole and fractional
//! phases, a tone keeps its level and frequency from every rate a Mac
//! device runs at and leaves no spur, and the timing holds.
//! Swift: `Tests/StenoAudioTests/RateConverterTests.swift`.

// Test arithmetic: sample counts and dB values cast freely, and sample
// rates compare exactly on purpose.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::doc_markdown,
    clippy::cast_lossless
)]

mod common;

use steno_audio::SAMPLE_RATE;
use steno_audio::codec::sinc::SincResampler;
use steno_audio::realtime::RateConverter;
use steno_audio::testing::AudioFixtures;

use common::{frequency, level_against_sine};

/// `seconds` of a sine of `hertz` at amplitude 0.5, sampled at `rate`.
fn tone(hertz: f64, rate: f64, seconds: f64) -> Vec<f32> {
    let increment = AudioFixtures::phase_increment_at(hertz, rate);
    let mut phase: u32 = 0;
    (0..(rate * seconds) as usize)
        .map(|_| {
            let sample = (0.5 * AudioFixtures::sine(phase)) as f32;
            phase = phase.wrapping_add(increment);
            sample
        })
        .collect()
}

/// `input` through one converter in calls of `chunks` samples in turn.
fn convert(rate: f64, input: &[f32], chunks: &[usize]) -> Vec<f32> {
    let largest = chunks.iter().copied().max().unwrap();
    let mut converter = RateConverter::new(rate, SAMPLE_RATE, largest);
    let mut scratch = vec![0.0f32; converter.max_output()];
    let mut output = Vec::new();
    let mut offset = 0;
    for chunk in chunks.iter().cycle() {
        if offset == input.len() {
            break;
        }
        let end = (offset + chunk).min(input.len());
        let written = converter.process(&input[offset..end], &mut scratch);
        output.extend_from_slice(&scratch[..written]);
        offset = end;
    }
    output
}

#[test]
fn chunked_input_converts_exactly_as_one_pass() {
    for rate in [16_000.0, 24_000.0, 44_100.0, 96_000.0] {
        let input = tone(1_000.0, rate, 1.0);
        let whole = convert(rate, &input, &[input.len()]);
        let chunked = convert(rate, &input, &[1, 7, 240, 100, 33, 512]);
        assert_eq!(whole, chunked, "{rate} Hz");
    }
}

#[test]
fn the_output_follows_the_ratio_less_half_a_window() {
    for rate in [8_000.0, 16_000.0, 24_000.0, 44_100.0, 96_000.0, 192_000.0] {
        let input = tone(1_000.0, rate, 1.0);
        let output = convert(rate, &input, &[480]);
        // The last `TAPS / 2` inputs wait for a next call that never comes.
        let held = (SincResampler::TAPS / 2) as f64 * SAMPLE_RATE / rate;
        let expected = SAMPLE_RATE - held;
        assert!(
            (output.len() as f64 - expected).abs() <= 2.0,
            "{rate} Hz: {} samples, expected about {expected}",
            output.len()
        );
    }
}

/// At 24 kHz every output sits on a whole phase; at 44.1 and 16 kHz most
/// sit between two, where the adjacent phases are blended.
#[test]
fn the_stream_matches_the_offline_sinc_resampler() {
    for rate in [24_000.0, 44_100.0, 16_000.0] {
        let input = tone(440.0, rate, 0.5);
        let streamed = convert(rate, &input, &[240]);
        let offline = SincResampler::new(rate, SAMPLE_RATE).resample(&input);
        assert!(streamed.len() > 23_000, "{rate} Hz: {}", streamed.len());
        for (index, (a, b)) in streamed.iter().zip(&offline).enumerate() {
            assert!(
                (a - b).abs() < 1e-4,
                "{rate} Hz, sample {index}: {a} against {b}"
            );
        }
    }
}

/// What a 1 kHz tone leaves once the sine fitted to it is removed, in dB
/// against the tone: the converter's spurs and images, and nothing of the
/// tone's own level.
#[test]
fn a_tone_from_44_1_khz_leaves_no_spur() {
    let rate = 44_100.0;
    let output = convert(rate, &tone(1_000.0, rate, 1.0), &[480]);
    // The generator's frequency, as its 32-bit phase step gives it.
    let hertz =
        f64::from(AudioFixtures::phase_increment_at(1_000.0, rate)) * rate / 4_294_967_296.0;
    // 800 whole periods of the steady part.
    let steady = &output[1_000..39_400];
    let (mut sine, mut cosine) = (0.0f64, 0.0f64);
    let angle = |index: usize| 2.0 * std::f64::consts::PI * hertz * index as f64 / SAMPLE_RATE;
    for (offset, sample) in steady.iter().enumerate() {
        let at = angle(1_000 + offset);
        sine += f64::from(*sample) * at.sin();
        cosine += f64::from(*sample) * at.cos();
    }
    let count = steady.len() as f64;
    let (sine, cosine) = (2.0 * sine / count, 2.0 * cosine / count);
    let residual: f64 = steady
        .iter()
        .enumerate()
        .map(|(offset, sample)| {
            let at = angle(1_000 + offset);
            (f64::from(*sample) - sine * at.sin() - cosine * at.cos()).powi(2)
        })
        .sum::<f64>()
        / count;
    let tone_power = sine.hypot(cosine).powi(2) / 2.0;
    let decibels = 10.0 * (residual / tone_power).log10();
    assert!(decibels < -80.0, "the residual is {decibels} dB");
}

#[test]
fn a_tone_keeps_its_level_and_frequency_from_every_device_rate() {
    for rate in [8_000.0, 16_000.0, 24_000.0, 44_100.0, 96_000.0, 192_000.0] {
        let output = convert(rate, &tone(1_000.0, rate, 1.0), &[480]);
        let steady = &output[1_000..40_000];
        let error = level_against_sine(steady, 0.5);
        assert!(error.abs() < 0.1, "{rate} Hz: {error} dB");
        let hertz = frequency(steady, SAMPLE_RATE);
        assert!((hertz - 1_000.0).abs() < 2.0, "{rate} Hz: {hertz} Hz");
    }
}

#[test]
fn content_above_48k_nyquist_is_rejected() {
    let output = convert(96_000.0, &tone(30_000.0, 96_000.0, 1.0), &[480]);
    let rejection = level_against_sine(&output[1_000..40_000], 0.5);
    assert!(rejection < -50.0, "30 kHz aliases at {rejection} dB");
}

#[test]
fn an_impulse_lands_on_its_output_sample() {
    // An impulse at device sample 100 lands on output sample 200 at 24 kHz
    // and 300 at 16 kHz: the lanes keep their timing.
    for (rate, at) in [(24_000.0, 200), (16_000.0, 300)] {
        let mut input = vec![0.0f32; 1_000];
        input[100] = 1.0;
        let output = convert(rate, &input, &[160]);
        let peak = output
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0;
        assert_eq!(peak, at, "{rate} Hz");
    }
}

#[test]
fn reset_starts_a_new_signal() {
    let input = tone(1_000.0, 24_000.0, 0.1);
    let mut converter = RateConverter::new(24_000.0, SAMPLE_RATE, input.len());
    let mut first = vec![0.0f32; converter.max_output()];
    let mut second = vec![0.0f32; converter.max_output()];
    let written = converter.process(&input, &mut first);
    converter.reset();
    assert_eq!(converter.process(&input, &mut second), written);
    assert_eq!(first, second);
}

#[test]
fn only_rates_from_8_to_192_khz_are_supported() {
    assert!(RateConverter::supports(8_000.0));
    assert!(RateConverter::supports(24_000.0));
    assert!(RateConverter::supports(192_000.0));
    assert!(!RateConverter::supports(0.0));
    assert!(!RateConverter::supports(4_000.0));
    assert!(!RateConverter::supports(384_000.0));
    assert!(!RateConverter::supports(f64::NAN));
}
