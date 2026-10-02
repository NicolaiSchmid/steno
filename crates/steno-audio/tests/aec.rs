//! The Speex canceller on the synthetic echo fixtures, with the ERLE table
//! `steno dev aec-bench --synthetic` prints.
//! Swift: `Tests/StenoAudioTests/SpeexEchoCancellerTests.swift`.

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

use std::fmt::Write as _;
use std::sync::OnceLock;

use steno_audio::aec::{
    EchoCancellerError, EchoMetrics, PassthroughEchoCanceller, SpeexEchoCanceller,
};
use steno_audio::testing::AudioFixtures;
use steno_core::EchoCanceller;

const SECONDS: f64 = 6.0;
const FRAME: usize = 480;
const RATE: usize = 48_000;

struct Material {
    far: Vec<f32>,
    room: Vec<f32>,
    echo_only: Vec<f32>,
}

fn material() -> &'static Material {
    static MATERIAL: OnceLock<Material> = OnceLock::new();
    MATERIAL.get_or_init(|| {
        let far = AudioFixtures::speech_like_far(SECONDS);
        let room = AudioFixtures::room_impulse_response();
        let echo_only = AudioFixtures::echo_mic(&far, &room);
        Material {
            far,
            room,
            echo_only,
        }
    })
}

fn one_decimal(value: f32) -> f32 {
    (value * 10.0).round() / 10.0
}

/// The parity table from `.plans/spikes/2026-10-01-spike-rust-capture.md`:
/// Swift's `aec-bench --synthetic` (CSpeex) and the vendored Speex agree to
/// the printed precision. Re-asserted here so a change to the fixtures, the
/// build flags or the canceller's controls shows up as a number.
#[test]
fn erle_table_matches_the_swift_bench() {
    let m = material();
    let mut aec = SpeexEchoCanceller::new(48_000.0, FRAME).unwrap();
    assert_eq!(aec.tail_length(), 9_600);
    let out = EchoMetrics::run(&mut aec, &m.echo_only, &m.far, FRAME);
    assert_eq!(out.len(), m.echo_only.len());

    let expected_per_second = [5.1f32, 15.7, 20.2, 22.7, 25.7, 26.8];
    let mut table = String::new();
    for (second, expected) in expected_per_second.iter().enumerate() {
        let erle = EchoMetrics::erle(&m.echo_only, &out, second * RATE..(second + 1) * RATE);
        let _ = writeln!(table, "  {second:3} s: ERLE {erle:5.1} dB");
        assert!(
            (one_decimal(erle) - expected).abs() <= 0.1 + 1e-4,
            "second {second}: ERLE {erle:.2} dB, Swift printed {expected}"
        );
    }
    let overall = EchoMetrics::erle(&m.echo_only, &out, 0..out.len());
    let after_three = EchoMetrics::erle(&m.echo_only, &out, 3 * RATE..out.len());
    let mic = EchoMetrics::decibels(EchoMetrics::rms(&m.echo_only[..out.len()]));
    let far = EchoMetrics::decibels(EchoMetrics::rms(&m.far[..out.len()]));
    let processed = EchoMetrics::decibels(EchoMetrics::rms(&out));
    println!(
        "engine: speex (vendored, rust), tail 200 ms, {} frames\nmic {mic:.1} dBFS, far {far:.1} dBFS, processed {processed:.1} dBFS\n{table}ERLE overall {overall:.1} dB, after 3 s {after_three:.1} dB",
        out.len()
    );
    assert!(
        (one_decimal(overall) - 12.3).abs() <= 0.1 + 1e-4,
        "overall {overall:.2}"
    );
    assert!(
        (one_decimal(after_three) - 24.7).abs() <= 0.1 + 1e-4,
        "after 3 s {after_three:.2}"
    );
    assert!(
        (one_decimal(mic) - -24.5).abs() <= 0.1 + 1e-4,
        "mic {mic:.2}"
    );
    assert!(
        (one_decimal(far) - -18.6).abs() <= 0.1 + 1e-4,
        "far {far:.2}"
    );
    assert!(
        (one_decimal(processed) - -36.9).abs() <= 0.1 + 1e-4,
        "processed {processed:.2}"
    );
    assert!(after_three >= 20.0, "ERLE {after_three} dB");
}

#[test]
fn double_talk_keeps_the_near_end_sweep() {
    let m = material();
    // Nobody talks for three seconds, then a local sweep joins the echo.
    let sweep = AudioFixtures::sweep(300.0, 3_000.0, 3.0, 0.3);
    let mut near_end = m.echo_only.clone();
    for (index, sample) in sweep.iter().enumerate() {
        near_end[3 * RATE + index] += sample;
    }
    let mut aec = SpeexEchoCanceller::new(48_000.0, FRAME).unwrap();
    let out = EchoMetrics::run(&mut aec, &near_end, &m.far, FRAME);
    let range = (3.5 * RATE as f64) as usize..6 * RATE;
    let sweep_level = EchoMetrics::decibels(EchoMetrics::rms(
        &sweep[range.start - 3 * RATE..range.end - 3 * RATE],
    ));
    let output_level = EchoMetrics::decibels(EchoMetrics::rms(&out[range]));
    assert!(
        (output_level - sweep_level).abs() <= 3.0,
        "sweep {sweep_level} dBFS, output {output_level} dBFS"
    );
}

#[test]
fn reset_forgets_the_filter_and_passthrough_does_nothing() {
    let m = material();
    let mut aec = SpeexEchoCanceller::new(48_000.0, FRAME).unwrap();
    let two_seconds = &m.echo_only[..2 * RATE];
    let far_two = &m.far[..2 * RATE];
    let converged = EchoMetrics::run(&mut aec, two_seconds, far_two, FRAME);
    aec.reset();
    let after_reset = EchoMetrics::run(&mut aec, two_seconds, far_two, FRAME);
    // The first half-second after a reset cancels less than the same half
    // second of a converged filter would.
    let range = 0..RATE / 2;
    let converged_again = EchoMetrics::run(&mut aec, two_seconds, far_two, FRAME);
    let early = EchoMetrics::erle(two_seconds, &after_reset, range.clone());
    let warm = EchoMetrics::erle(two_seconds, &converged_again, range);
    assert!(warm > early, "warm {warm} dB vs after reset {early} dB");
    assert_eq!(converged.len(), two_seconds.len());

    let mut passthrough = PassthroughEchoCanceller::new(48_000.0, FRAME);
    let unchanged = EchoMetrics::run(&mut passthrough, two_seconds, far_two, FRAME);
    assert_eq!(unchanged, two_seconds);
}

#[test]
fn rejects_impossible_shapes_and_pads_short_buffers() {
    assert!(matches!(
        SpeexEchoCanceller::with_tail(48_000.0, 480, 100, -40, -15),
        Err(EchoCancellerError::InitialisationFailed(_))
    ));
    assert!(matches!(
        SpeexEchoCanceller::with_tail(48_000.0, 0, 100, -40, -15),
        Err(EchoCancellerError::InitialisationFailed(_))
    ));
    let mut aec = SpeexEchoCanceller::new(48_000.0, 480).unwrap();
    let loud = [2.0f32; 100];
    let mut out = [9.0f32; 480];
    aec.process(&loud, &loud, &mut out);
    assert!(out.iter().all(|s| (-1.0..=1.0).contains(s)));
    assert_ne!(out[479], 9.0, "every output sample is written");
}

#[test]
fn fixtures_are_deterministic() {
    let m = material();
    assert_eq!(
        AudioFixtures::speech_like_far(0.1),
        AudioFixtures::speech_like_far(0.1)
    );
    assert_eq!(AudioFixtures::room_impulse_response(), m.room);
    assert_eq!(m.room[0], 1.0);
    assert_eq!(m.room.len(), 4_800);
    assert!(m.room[4_799].abs() < 0.001);
    let tone = AudioFixtures::tone(1_000.0, 1.0, 0.5);
    assert_eq!(tone.len(), 48_000);
    assert!((EchoMetrics::decibels(EchoMetrics::rms(&tone)) - -9.03).abs() < 0.05);
    let far_level = EchoMetrics::decibels(EchoMetrics::rms(&m.far));
    assert!(
        far_level > -30.0 && far_level < -6.0,
        "far-end at {far_level} dBFS"
    );
    let echo_level = EchoMetrics::decibels(EchoMetrics::rms(&m.echo_only));
    assert!(
        echo_level > -40.0 && echo_level < -6.0,
        "echo at {echo_level} dBFS"
    );
}

#[test]
fn goertzel_reads_a_full_scale_sine_as_one() {
    let tone = AudioFixtures::tone(1_000.0, 1.0, 1.0);
    assert!((EchoMetrics::tone_level(&tone, 1_000.0, 48_000.0) - 1.0).abs() < 0.01);
    assert!(EchoMetrics::tone_level(&tone, 3_000.0, 48_000.0) < 0.01);
}
