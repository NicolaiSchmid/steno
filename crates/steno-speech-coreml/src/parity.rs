//! The parity harness: every `<name>.wav` of the calibration corpus
//! through the pipeline, scored against `<name>.parakeet-v3.json`, the
//! `RawSegment` array Steno's Swift engine produced for the same file
//! (`steno bakeoff` output). Reports WER of the Rust text against the
//! Swift text, word-start agreement where the words match, wall time and
//! RTFx per file, and the load average around the run, because the
//! decoder loop is thousands of small CoreML calls whose cost is CPU
//! scheduling.
//!
//! Manual: `cargo run --release -p steno-speech-coreml --bin
//! steno-coreml-parity -- ~/steno-spikes/corpus ~/steno-spikes/baseline-bakeoff`
//! on Forge, or the ignored test in `tests/parity.rs` with
//! `STENO_CALIBRATION_CORPUS` set.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use steno_core::RawSegment;

use crate::SpeechError;
use crate::backend::Backend;
use crate::chunking::sample_seconds;
use crate::engine::default_model_directory;
use crate::pipeline::{Config, Stats, Transcriber};
use crate::segments::raw_segments;
use crate::wav::read_mono_16k;
use crate::wer::{TimedText, TimingAgreement, WordErrors, timing_agreement, word_errors};

/// How to run the harness.
#[derive(Debug, Clone)]
pub struct Options {
    /// The model directory; the app's by default.
    pub models: Option<PathBuf>,
    /// Where to write `<name>.rust.json`; nothing is written when unset.
    pub out: Option<PathBuf>,
    /// Parallel windows.
    pub concurrency: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            models: None,
            out: None,
            concurrency: Config::default().concurrency,
        }
    }
}

/// One corpus file's result.
#[derive(Debug, Clone)]
pub struct FileResult {
    /// File stem shared by the WAV and the baseline JSON.
    pub name: String,
    /// Audio length.
    pub audio_seconds: f64,
    /// Transcription wall time, model load excluded.
    pub wall_seconds: f64,
    /// Merged tokens.
    pub tokens: usize,
    /// Counters and timings of the run.
    pub stats: Stats,
    /// Word errors of the Rust text against the Swift text.
    pub errors: WordErrors,
    /// Word-start agreement where the text matches.
    pub timing: TimingAgreement,
}

impl FileResult {
    /// Audio seconds per wall second.
    #[must_use]
    pub fn rtfx(&self) -> f64 {
        if self.wall_seconds > 0.0 {
            self.audio_seconds / self.wall_seconds
        } else {
            0.0
        }
    }
}

/// The whole run.
#[derive(Debug, Clone)]
pub struct Report {
    /// One row per corpus file, in name order.
    pub files: Vec<FileResult>,
    /// Word errors summed over the corpus.
    pub total: WordErrors,
    /// Time to load the four models and the vocabulary.
    pub model_load_seconds: f64,
    /// Parallel windows.
    pub concurrency: usize,
    /// `vm.loadavg` before the first file.
    pub load_before: String,
    /// `vm.loadavg` after the last file.
    pub load_after: String,
}

impl Report {
    /// Mean of the per-file WERs (the plan's target is this mean).
    #[must_use]
    pub fn mean_file_wer(&self) -> f64 {
        self.mean(|file| file.errors.rate())
    }

    /// Mean RTFx over the files.
    #[must_use]
    pub fn mean_rtfx(&self) -> f64 {
        self.mean(FileResult::rtfx)
    }

    /// Mean of `value` over the files; zero without files.
    fn mean(&self, value: impl Fn(&FileResult) -> f64) -> f64 {
        if self.files.is_empty() {
            return 0.0;
        }
        // File counts are tiny.
        #[allow(clippy::cast_precision_loss)]
        let count = self.files.len() as f64;
        self.files.iter().map(value).sum::<f64>() / count
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "models loaded in {:.2} s, {} parallel windows, load average before {} after {}",
            self.model_load_seconds, self.concurrency, self.load_before, self.load_after
        )?;
        writeln!(f)?;
        writeln!(
            f,
            "| File | Audio s | Wall s | RTFx | Windows | Tokens | Recoveries accepted/tried | Repaired tokens/probes | Swift words | Rust words | Edits | WER vs Swift | Starts within 10 ms |"
        )?;
        writeln!(
            f,
            "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
        )?;
        for file in &self.files {
            writeln!(
                f,
                "| {} | {:.2} | {:.2} | {:.0} | {} | {} | {}/{} | {}/{} | {} | {} | {} | {:.2} % | {}/{} ({:.1} %) |",
                file.name,
                file.audio_seconds,
                file.wall_seconds,
                file.rtfx(),
                file.stats.windows,
                file.tokens,
                file.stats.recoveries_accepted,
                file.stats.recoveries_tried,
                file.stats.repaired_tokens,
                file.stats.repair_probes,
                file.errors.reference_words,
                file.errors.hypothesis_words,
                file.errors.edits,
                file.errors.rate() * 100.0,
                file.timing.within_10ms,
                file.timing.matched,
                file.timing.fraction_within_10ms() * 100.0,
            )?;
        }
        writeln!(
            f,
            "| mean / all | | | {:.0} | | | | | {} | {} | {} | {:.2} % (mean of files {:.2} %) | |",
            self.mean_rtfx(),
            self.total.reference_words,
            self.total.hypothesis_words,
            self.total.edits,
            self.total.rate() * 100.0,
            self.mean_file_wer() * 100.0,
        )
    }
}

/// The 1-minute, 5-minute and 15-minute load averages as the kernel
/// reports them, or `?` when `sysctl` is unavailable.
#[must_use]
pub fn load_average() -> String {
    Command::new("sysctl")
        .args(["-n", "vm.loadavg"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map_or_else(
            || "?".to_owned(),
            |output| {
                String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .trim_matches(|c| c == '{' || c == '}')
                    .trim()
                    .to_owned()
            },
        )
}

/// The baseline segments for `name`.
fn read_baseline(baseline_dir: &Path, name: &str) -> Result<Vec<RawSegment>, SpeechError> {
    let path = baseline_dir.join(format!("{name}.parakeet-v3.json"));
    let text = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&text)?)
}

fn joined_text(segments: &[RawSegment]) -> String {
    segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn timed_words(segments: &[RawSegment]) -> Vec<TimedText> {
    segments
        .iter()
        .flat_map(|segment| segment.word_timings.iter().flatten())
        .map(|word| TimedText {
            word: word.word.clone(),
            start: word.start,
        })
        .collect()
}

/// Run the corpus. Files are the `*.wav` of `corpus_dir` in name order;
/// a file without a baseline is an error.
pub fn run(
    corpus_dir: &Path,
    baseline_dir: &Path,
    options: &Options,
) -> Result<Report, SpeechError> {
    let models = options
        .models
        .clone()
        .unwrap_or_else(default_model_directory);
    let load_before = load_average();
    let backend = Backend::load(&models)?;
    let model_load_seconds = backend.load_seconds();
    let transcriber = Transcriber::with_config(
        backend,
        Config {
            concurrency: options.concurrency,
            ..Config::default()
        },
    );
    eprintln!(
        "models loaded from {} in {model_load_seconds:.2} s",
        models.display()
    );

    let mut wavs: Vec<PathBuf> = std::fs::read_dir(corpus_dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
        })
        .collect();
    wavs.sort();
    if let Some(out) = &options.out {
        std::fs::create_dir_all(out)?;
    }

    let mut files = Vec::with_capacity(wavs.len());
    let mut total = WordErrors::default();
    for wav in &wavs {
        let name = wav
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let baseline = read_baseline(baseline_dir, &name)?;
        let audio = read_mono_16k(wav)?;
        let audio_seconds = sample_seconds(audio.len());

        let started = Instant::now();
        let transcript = transcriber.transcribe(&audio)?;
        let wall_seconds = started.elapsed().as_secs_f64();

        let tokens = transcript.tokens;
        let segments = raw_segments(&tokens, transcriber.vocab(), audio_seconds);
        let errors = word_errors(&joined_text(&baseline), &joined_text(&segments));
        let timing = timing_agreement(&timed_words(&baseline), &timed_words(&segments));
        total.add(errors);

        if let Some(out) = &options.out {
            let json = serde_json::to_string_pretty(&segments)?;
            std::fs::write(out.join(format!("{name}.rust.json")), json)?;
        }
        let result = FileResult {
            name,
            audio_seconds,
            wall_seconds,
            tokens: tokens.len(),
            stats: transcript.stats,
            errors,
            timing,
        };
        eprintln!(
            "{}: {wall_seconds:.2} s, RTFx {:.0}, WER {:.2} %, {} windows, {} tokens",
            result.name,
            result.rtfx(),
            errors.rate() * 100.0,
            result.stats.windows,
            result.tokens
        );
        files.push(result);
    }
    Ok(Report {
        files,
        total,
        model_load_seconds,
        concurrency: options.concurrency,
        load_before,
        load_after: load_average(),
    })
}
