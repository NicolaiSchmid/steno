//! Gate G3 of `.plans/2026-10-01-cross-platform-speech-stack.md`: the seven
//! calibration calls against `truth.json`. Ignored unless
//! `STENO_CALIBRATION_CORPUS` points at the corpus directory
//! (`audio/<id>/system.wav` and `truth.json`, read only). Analyses each
//! lane once and sweeps the clustering cut, with and without refinement,
//! and prints a Markdown table per backend for the PR.
//!
//! Environment:
//! - `STENO_CALIBRATION_CORPUS`: the corpus directory (required).
//! - `STENO_MODELS_DIR`: where the ONNX models live or are downloaded to
//!   (default: a `steno-diarize-models` directory under the system temp dir).
//! - `STENO_DIARIZE_BACKEND`: `onnx` (default) or `coreml`.
//! - `STENO_COREML_MODELS`: `FluidAudio`'s `speaker-diarization` directory
//!   for the `CoreML` backend.
//! - `STENO_DIARIZE_THRESHOLDS`: comma-separated cosine-distance cuts
//!   (default `0.20,0.26,0.32,0.38,0.44,0.50,0.60`).
//! - `STENO_DIARIZE_THREADS`: ONNX Runtime intra-op threads (default 4).
//! - `STENO_DIARIZE_FILES`: comma-separated ids to restrict the run, each
//!   a full id or its first eight characters; an id that matches no lane
//!   fails the run rather than silently running fewer lanes.
//!
//! Run: `STENO_CALIBRATION_CORPUS=~/steno-calibration cargo test -p steno-diarize
//! --release --test calibration -- --ignored --nocapture`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use steno_core::AudioBuffer16k;
use steno_diarize::{DiarizerConfig, Pipeline, TensorBackend};

/// The remote speaker count per call from `truth.json`, whatever shape
/// the file has: an object keyed by id with a number or an object holding
/// a count under a known key, or a list of objects with an `id`.
fn truth(path: &Path) -> BTreeMap<String, usize> {
    let text = std::fs::read_to_string(path).expect("truth.json readable");
    let value: serde_json::Value = serde_json::from_str(&text).expect("truth.json is JSON");
    let mut result = BTreeMap::new();
    let count_of = |value: &serde_json::Value| -> Option<usize> {
        if let Some(number) = value.as_u64() {
            return usize::try_from(number).ok();
        }
        let object = value.as_object()?;
        for key in [
            "remote_speakers",
            "remoteSpeakers",
            "remote",
            "speakers",
            "speaker_count",
            "speakerCount",
            "count",
            "expected",
        ] {
            if let Some(number) = object.get(key).and_then(serde_json::Value::as_u64) {
                return usize::try_from(number).ok();
            }
            if let Some(list) = object.get(key).and_then(serde_json::Value::as_array) {
                return Some(list.len());
            }
        }
        None
    };
    match &value {
        serde_json::Value::Object(map) => {
            for (id, entry) in map {
                if let Some(count) = count_of(entry) {
                    result.insert(id.clone(), count);
                }
            }
            // A wrapper object such as `{"calls": {...}}` or `{"calls": [...]}`.
            if result.is_empty() {
                for entry in map.values() {
                    result.extend(truth_value(entry, &count_of));
                }
            }
        }
        other => result.extend(truth_value(other, &count_of)),
    }
    result
}

fn truth_value(
    value: &serde_json::Value,
    count_of: &dyn Fn(&serde_json::Value) -> Option<usize>,
) -> BTreeMap<String, usize> {
    let mut result = BTreeMap::new();
    match value {
        serde_json::Value::Array(list) => {
            for entry in list {
                let id = entry
                    .get("id")
                    .or_else(|| entry.get("meeting"))
                    .or_else(|| entry.get("call"))
                    .and_then(serde_json::Value::as_str);
                if let (Some(id), Some(count)) = (id, count_of(entry)) {
                    result.insert(id.to_owned(), count);
                }
            }
        }
        serde_json::Value::Object(map) => {
            for (id, entry) in map {
                if let Some(count) = count_of(entry) {
                    result.insert(id.clone(), count);
                }
            }
        }
        _ => {}
    }
    result
}

fn read_wav(path: &Path) -> AudioBuffer16k {
    let mut reader = hound::WavReader::open(path).expect("wav opens");
    let spec = reader.spec();
    assert_eq!(
        spec.sample_rate,
        16_000,
        "{}: the corpus is 16 kHz",
        path.display()
    );
    let channels = usize::from(spec.channels.max(1));
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().map(|s| s.unwrap()).collect(),
        hound::SampleFormat::Int => {
            let scale = f32::from(spec.bits_per_sample.min(32)).exp2() / 2.0;
            reader
                .samples::<i32>()
                .map(|s| {
                    #[allow(clippy::cast_precision_loss)]
                    let value = s.unwrap() as f32 / scale;
                    value
                })
                .collect()
        }
    };
    let mono: Vec<f32> = samples
        .chunks(channels)
        .map(|frame| {
            #[allow(clippy::cast_precision_loss)]
            let mean = frame.iter().sum::<f32>() / frame.len() as f32;
            mean
        })
        .collect();
    AudioBuffer16k::new(mono)
}

fn backend() -> (String, Box<dyn TensorBackend>) {
    let kind = std::env::var("STENO_DIARIZE_BACKEND").unwrap_or_else(|_| "onnx".to_owned());
    match kind.as_str() {
        #[cfg(feature = "onnx")]
        "onnx" => {
            let dir = std::env::var_os("STENO_MODELS_DIR").map_or_else(
                || std::env::temp_dir().join("steno-diarize-models"),
                PathBuf::from,
            );
            let threads = std::env::var("STENO_DIARIZE_THREADS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(4);
            let store = steno_diarize::models::ModelStore::new(dir);
            let backend = steno_diarize::onnx::OnnxBackend::from_store(&store, threads)
                .expect("ONNX backend loads");
            (
                format!("ONNX Runtime, CPU, {threads} threads"),
                Box::new(backend),
            )
        }
        #[cfg(all(feature = "coreml", target_os = "macos"))]
        "coreml" => {
            let dir = std::env::var_os("STENO_COREML_MODELS")
                .map(PathBuf::from)
                .expect("STENO_COREML_MODELS points at FluidAudio's speaker-diarization directory");
            let backend =
                steno_diarize::coreml::CoreMlBackend::load(&dir).expect("CoreML backend loads");
            ("CoreML (FluidAudio models)".to_owned(), Box::new(backend))
        }
        other => panic!("STENO_DIARIZE_BACKEND={other} is not built into this binary"),
    }
}

fn load_average() -> String {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|text| text.split_whitespace().next().map(str::to_owned))
        .or_else(|| {
            std::process::Command::new("sysctl")
                .args(["-n", "vm.loadavg"])
                .output()
                .ok()
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .and_then(|text| text.split_whitespace().nth(1).map(str::to_owned))
        })
        .unwrap_or_else(|| "?".to_owned())
}

/// Whether `id` is one of the `requested` lanes: a full id or its first
/// eight characters, as the table prints them.
fn is_requested(id: &str, requested: &str) -> bool {
    id == requested || (requested.len() >= 8 && id.starts_with(requested))
}

/// The lanes `STENO_DIARIZE_FILES` restricts the run to, `None` for all;
/// an id that names no lane in `truth` fails the run.
fn requested_lanes(truth: &BTreeMap<String, usize>) -> Option<Vec<String>> {
    let requested: Vec<String> = std::env::var("STENO_DIARIZE_FILES")
        .ok()?
        .split(',')
        .map(|s| s.trim().to_owned())
        .collect();
    let unknown: Vec<&String> = requested
        .iter()
        .filter(|wanted| !truth.keys().any(|id| is_requested(id, wanted)))
        .collect();
    assert!(
        unknown.is_empty(),
        "STENO_DIARIZE_FILES names no lane in truth.json: {unknown:?}"
    );
    Some(requested)
}

#[test]
#[ignore = "needs the calibration corpus: STENO_CALIBRATION_CORPUS"]
fn g3_speaker_counts_against_truth() {
    let Some(corpus) = std::env::var_os("STENO_CALIBRATION_CORPUS").map(PathBuf::from) else {
        eprintln!("STENO_CALIBRATION_CORPUS unset; nothing to do");
        return;
    };
    let truth = truth(&corpus.join("truth.json"));
    assert!(!truth.is_empty(), "truth.json yielded no counts");
    let only = requested_lanes(&truth);
    let thresholds: Vec<f32> = std::env::var("STENO_DIARIZE_THRESHOLDS").ok().map_or_else(
        || vec![0.20, 0.26, 0.32, 0.38, 0.44, 0.50, 0.60],
        |list| {
            list.split(',')
                .map(|s| s.trim().parse().expect("threshold"))
                .collect()
        },
    );
    let (name, backend) = backend();
    let mut pipeline = Pipeline::new(backend, DiarizerConfig::default());

    let header: Vec<String> = thresholds.iter().map(|t| format!("{t:.2}")).collect();
    println!("\n### {name}\n");
    println!(
        "Counts per clustering cut (cosine distance); `refined` is after the refinement pass, `mapped` before it.\n"
    );
    println!(
        "| Call | Truth | Minutes | {} | Analysis s | Load |",
        header
            .iter()
            .map(|t| format!("{t} refined / mapped"))
            .collect::<Vec<_>>()
            .join(" | ")
    );
    println!(
        "|---|---:|---:|{}---:|---:|",
        "---:|".repeat(thresholds.len())
    );
    let mut gate: BTreeMap<String, Vec<bool>> = BTreeMap::new();
    for (id, expected) in &truth {
        if only
            .as_ref()
            .is_some_and(|list| !list.iter().any(|wanted| is_requested(id, wanted)))
        {
            continue;
        }
        let wav = corpus.join("audio").join(id).join("system.wav");
        if !wav.is_file() {
            eprintln!("{}: missing, skipped", wav.display());
            continue;
        }
        let audio = read_wav(&wav);
        let load = load_average();
        let started = Instant::now();
        let analysis = pipeline.analyze(&audio).expect("analysis");
        let analysis_seconds = started.elapsed().as_secs_f64();
        let mut cells = Vec::new();
        for threshold in &thresholds {
            let mapped = pipeline.map(&analysis, *threshold);
            let refined = pipeline.refine(&mapped, &audio).expect("refinement");
            let pass = if *expected == 1 {
                refined.clusters.len() == 1
            } else {
                refined.clusters.len().abs_diff(*expected) <= 1
            };
            gate.entry(format!("{threshold:.2}"))
                .or_default()
                .push(pass);
            cells.push(format!(
                "{}{} / {}",
                refined.clusters.len(),
                if pass { "" } else { " x" },
                mapped.clusters.len()
            ));
        }
        println!(
            "| {} | {} | {:.0} | {} | {:.0} | {} |",
            &id[..id.len().min(8)],
            expected,
            audio.duration() / 60.0,
            cells.join(" | "),
            analysis_seconds,
            load
        );
    }
    println!();
    for (threshold, results) in &gate {
        let passed = results.iter().filter(|p| **p).count();
        println!(
            "- cut {threshold}: {passed} of {} calls within the gate",
            results.len()
        );
    }
}
