//! Every bound `MetadataValidation` enforces before a partial file exists,
//! at the edge on both sides.

#![allow(
    clippy::assert_is_empty,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::large_futures,
    clippy::too_many_lines
)]

mod common;

use std::time::Duration;

use common::date;
use steno_core::{AudioFormat, RecordingMetadata};
use steno_handover::HandoverConfiguration;
use steno_handover::upload::MetadataValidation;
use uuid::Uuid;

fn configuration() -> HandoverConfiguration {
    HandoverConfiguration {
        service_name: "Test Mac".to_owned(),
        advertise: false,
        chunk_size: 1024 * 1024,
        inbox_directory: "/nonexistent/inbox".into(),
        pairing_window: Duration::from_secs(300),
        port: 0,
        read_timeout: Duration::from_secs(30),
    }
}

fn valid() -> RecordingMetadata {
    RecordingMetadata {
        recording_id: Uuid::new_v4(),
        started_at: date(1_790_000_000),
        duration_seconds: 61.5,
        byte_count: 3_000_000,
        sha256: vec![1; 32],
        chunk_size: 1024 * 1024,
        format: AudioFormat::M4aAac,
        device_name: "iPhone".to_owned(),
    }
}

fn problem(change: impl FnOnce(&mut RecordingMetadata)) -> Option<String> {
    let mut metadata = valid();
    change(&mut metadata);
    MetadataValidation::problem(&metadata, &configuration())
}

fn mentions(problem: Option<String>, field: &str) -> bool {
    problem.is_some_and(|text| text.contains(field))
}

#[test]
fn the_sample_is_accepted() {
    assert_eq!(problem(|_| {}), None);
}

#[test]
fn byte_count_is_one_to_four_gib() {
    assert!(mentions(problem(|m| m.byte_count = 0), "byteCount"));
    assert!(mentions(problem(|m| m.byte_count = -1), "byteCount"));
    assert_eq!(problem(|m| m.byte_count = 1), None);
    assert_eq!(
        problem(|m| m.byte_count = MetadataValidation::MAX_BYTE_COUNT),
        None
    );
    assert!(mentions(
        problem(|m| m.byte_count = MetadataValidation::MAX_BYTE_COUNT + 1),
        "byteCount"
    ));
}

#[test]
fn chunk_size_is_64_kib_to_the_limit() {
    assert!(mentions(
        problem(|m| m.chunk_size = MetadataValidation::MIN_CHUNK_SIZE - 1),
        "chunkSize"
    ));
    assert_eq!(
        problem(|m| m.chunk_size = MetadataValidation::MIN_CHUNK_SIZE),
        None
    );
    assert_eq!(problem(|m| m.chunk_size = configuration().chunk_size), None);
    assert!(mentions(
        problem(|m| m.chunk_size = configuration().chunk_size + 1),
        "chunkSize"
    ));
    assert!(mentions(problem(|m| m.chunk_size = 0), "chunkSize"));
}

#[test]
fn sha256_is_exactly_32_bytes() {
    assert!(mentions(problem(|m| m.sha256 = vec![1; 31]), "sha256"));
    assert!(mentions(problem(|m| m.sha256 = vec![1; 33]), "sha256"));
    assert!(mentions(problem(|m| m.sha256 = Vec::new()), "sha256"));
}

#[test]
fn duration_is_zero_to_seven_days_and_finite() {
    assert_eq!(problem(|m| m.duration_seconds = 0.0), None);
    assert_eq!(
        problem(|m| m.duration_seconds = MetadataValidation::MAX_DURATION_SECONDS),
        None
    );
    assert!(mentions(
        problem(|m| m.duration_seconds = -0.5),
        "durationSeconds"
    ));
    assert!(mentions(
        problem(|m| m.duration_seconds = MetadataValidation::MAX_DURATION_SECONDS + 1.0),
        "durationSeconds"
    ));
    assert!(mentions(
        problem(|m| m.duration_seconds = f64::NAN),
        "durationSeconds"
    ));
    assert!(mentions(
        problem(|m| m.duration_seconds = f64::INFINITY),
        "durationSeconds"
    ));
}

#[test]
fn device_name_is_one_to_128_characters_after_trimming() {
    assert!(mentions(
        problem(|m| m.device_name = String::new()),
        "deviceName"
    ));
    assert!(mentions(
        problem(|m| m.device_name = " \n\t".to_owned()),
        "deviceName"
    ));
    assert_eq!(problem(|m| m.device_name = "x".repeat(128)), None);
    assert!(mentions(
        problem(|m| m.device_name = "x".repeat(129)),
        "deviceName"
    ));
    assert_eq!(
        problem(|m| m.device_name = "  Nicolai's iPhone  ".to_owned()),
        None
    );
}

#[test]
fn only_phone_and_fixture_formats_are_accepted() {
    assert_eq!(problem(|m| m.format = AudioFormat::M4aAac), None);
    assert_eq!(problem(|m| m.format = AudioFormat::Wav16kInt16), None);
    assert!(mentions(
        problem(|m| m.format = AudioFormat::Caf48kFloat32),
        "format"
    ));
}

#[test]
fn the_first_problem_wins_and_the_disk_is_never_touched() {
    // Two problems: byteCount reports first, the order the phone can rely
    // on in logs. The inbox directory does not exist, so nothing was
    // created.
    let problem = problem(|m| {
        m.byte_count = 0;
        m.format = AudioFormat::Caf48kFloat32;
    });
    assert!(mentions(problem, "byteCount"));
    assert!(!configuration().inbox_directory.exists());
}
