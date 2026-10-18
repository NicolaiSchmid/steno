//! The engine's extra windows (`ChunkerConfig::decode_gaps`) on real speech,
//! ignored tests for a Mac with the models and `STENO_FLEURS_DIR` set: quiet
//! speech the detector misses must keep at least [`AT_LEAST`] words, and no
//! word may lie in silence or room noise. `STENO_EXTRA_WINDOWS_OUT` writes
//! each case as a WAV for the parity binary.

#![cfg(target_os = "macos")]

#[path = "../../steno-speech/tests/common/crafted.rs"]
mod crafted;

use std::sync::OnceLock;

use crafted::{
    Case, EDGE_SECONDS, chunk_count, extra_windows, fleurs, quiet_cases, quiet_room, time,
    words_inside, write_out,
};
use steno_speech::Transcript;
use steno_speech_coreml::CoreMlParakeetEngine;

/// Words each quiet stretch keeps, at -20 and at -26 dB. The 6.65 s gap is
/// pinned at 12: 1.3 s of it is under `MIN_GAP_SECONDS`, so no extra window
/// decodes that part.
const AT_LEAST: [(&str, usize); 4] = [("lead", 14), ("gap6", 12), ("gap9", 19), ("tail", 14)];

fn engine() -> &'static CoreMlParakeetEngine {
    static ENGINE: OnceLock<CoreMlParakeetEngine> = OnceLock::new();
    ENGINE.get_or_init(CoreMlParakeetEngine::in_default_directory)
}

fn transcribe(case: &Case) -> Transcript {
    write_out(case);
    engine()
        .transcribe_samples(&case.samples, None)
        .expect("the engine transcribes")
}

#[test]
#[ignore = "needs the Parakeet models and FLEURS German (Forge)"]
fn quiet_speech_the_chunks_leave_out_comes_through() {
    let Some(dir) = fleurs() else {
        return;
    };
    let mut failures = Vec::new();
    for (position, db, case) in quiet_cases(&dir) {
        let transcript = transcribe(&case);
        let words = words_inside(&transcript, &case.stretches[0], 0.0).len();
        let decoded_by_gaps = extra_windows(&transcript)
            .any(|g| g.start < case.stretches[0].end && case.stretches[0].start < g.end);
        println!(
            "{}: {words} words in the quiet stretch, {} chunks, {} extra windows{}",
            case.name,
            chunk_count(&transcript),
            extra_windows(&transcript).count(),
            if decoded_by_gaps {
                ", some over it"
            } else {
                ""
            }
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
#[ignore = "needs the Parakeet models and FLEURS German (Forge)"]
fn silence_and_room_noise_the_chunks_leave_out_add_no_word() {
    let Some(dir) = fleurs() else {
        return;
    };
    for noise in [false, true] {
        let case = quiet_room(&dir, noise);
        let transcript = transcribe(&case);
        for stretch in &case.stretches {
            // The chunks reach into the stretch; an extra window decodes the rest.
            assert!(
                !noise
                    || extra_windows(&transcript)
                        .any(|g| g.start < stretch.end && stretch.start < g.end),
                "{}: no extra window over {stretch:?}: spans {:?}",
                case.name,
                transcript.spans
            );
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
            extra_windows(&transcript).count()
        );
    }
}
