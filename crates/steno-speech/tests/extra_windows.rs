//! The ONNX engine's settings ([`pipeline_config`]) over the crafted cases
//! of `tests/common/crafted.rs`: quiet speech the detector does not hear,
//! which the first chunk keeps in the lead-in and the tail's extra window
//! keeps in the tail, and silence and room noise, which must add no word.
//! They need the model files and FLEURS German, so they skip with a
//! message unless `STENO_MODELS_DIR` and `STENO_FLEURS_DIR` point at them;
//! CI never has them.
//!
//! ```text
//! STENO_MODELS_DIR=... STENO_FLEURS_DIR=... \
//! cargo test --release -p steno-speech --test extra_windows -- --nocapture --test-threads=1
//! ```

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::assert_is_empty
)]

mod common;
#[path = "common/crafted.rs"]
mod crafted;

use std::sync::{Mutex, OnceLock};

use crafted::{
    Case, EDGE_SECONDS, chunk_count, extra_windows, quiet_cases, quiet_room, time, words_inside,
    write_out,
};
use steno_speech::engine::pipeline_config;
use steno_speech::{
    OnnxBackend, OnnxOptions, OnnxSpeechEngine, Transcriber, Transcript, VadConfig,
};

const MODELS: &str = "the fp32 export and Silero VAD";

/// Words the quiet lead-in and tail keep, at -20 and at -26 dB (on atlas,
/// A16b in `.plans/2026-10-07-stable-promotion.md`); the defaults kept 12
/// and 0 of the lead-in and none of the tail. A quiet stretch in a long
/// pause between two loud ones keeps none, as before: the engine skips
/// long pauses (the 6.65 s and 9.1 s gaps, `gap6` and `gap9`).
const AT_LEAST: [(&str, usize); 2] = [("lead", 15), ("tail", 14)];

fn transcriber() -> Option<&'static Mutex<Transcriber<OnnxBackend>>> {
    static TRANSCRIBER: OnceLock<Option<Mutex<Transcriber<OnnxBackend>>>> = OnceLock::new();
    TRANSCRIBER
        .get_or_init(|| {
            let Some(models) = common::installed_store() else {
                common::skip("STENO_MODELS_DIR", MODELS);
                return None;
            };
            let transcriber = OnnxSpeechEngine::load_installed(
                &models,
                &OnnxOptions::default(),
                pipeline_config(),
                VadConfig::default(),
            )
            .expect("open the export and Silero");
            Some(Mutex::new(transcriber))
        })
        .as_ref()
}

fn transcribe(transcriber: &Mutex<Transcriber<OnnxBackend>>, case: &Case) -> Transcript {
    write_out(case);
    let mut transcriber = transcriber
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    transcriber
        .transcribe(&case.samples, None)
        .expect("the engine transcribes")
}

#[test]
fn quiet_speech_the_chunks_may_leave_out_comes_through() {
    let Some(transcriber) = transcriber() else {
        return;
    };
    let Some(dir) = crafted::fleurs() else {
        return;
    };
    let mut failures = Vec::new();
    for (position, db, case) in quiet_cases(&dir) {
        let transcript = transcribe(transcriber, &case);
        let words = words_inside(&transcript, &case.stretches[0], 0.0).len();
        println!(
            "{}: {words} words in the quiet stretch, {} chunks, {} extra windows",
            case.name,
            chunk_count(&transcript),
            extra_windows(&transcript).count(),
        );
        let least = AT_LEAST
            .iter()
            .find(|(name, _)| *name == position)
            .map_or(0, |&(_, n)| n);
        if db != 0.0 && words < least {
            failures.push(format!("{}: {words}, at least {least}", case.name));
        }
    }
    assert!(failures.is_empty(), "quiet words lost: {failures:?}");
}

#[test]
fn silence_and_room_noise_the_chunks_leave_out_add_no_word() {
    let Some(transcriber) = transcriber() else {
        return;
    };
    let Some(dir) = crafted::fleurs() else {
        return;
    };
    for noise in [false, true] {
        let case = quiet_room(&dir, noise);
        let transcript = transcribe(transcriber, &case);
        for stretch in &case.stretches {
            let words = words_inside(&transcript, stretch, EDGE_SECONDS);
            assert!(
                words.is_empty(),
                "{}: {} words in {:.2} to {:.2} s",
                case.name,
                words.len(),
                time(stretch.start),
                time(stretch.end)
            );
        }
        println!(
            "{}: {} words, {} chunks, {} extra windows, none in the stretches",
            case.name,
            transcript.words.len(),
            chunk_count(&transcript),
            extra_windows(&transcript).count(),
        );
    }
}
