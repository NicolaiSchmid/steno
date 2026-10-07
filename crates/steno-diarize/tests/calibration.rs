//! Gate G3 of `.plans/2026-10-01-cross-platform-speech-stack.md`: the seven
//! calibration calls against `truth.json`. Ignored unless
//! `STENO_CALIBRATION_CORPUS` points at the corpus directory
//! (`audio/<id>/system.wav` and `truth.json`, read only). Analyses each
//! lane once and sweeps the clustering cut, with and without refinement,
//! and prints a Markdown table per backend for the PR. The run fails when
//! any lane misses the gate at [`DEFAULT_CLUSTERING_THRESHOLD`] or has no
//! audio in the corpus, and a run over every lane fails when a lane's
//! audio has no count in `truth.json`; the other cuts are reported, not
//! asserted.
//!
//! Environment:
//! - `STENO_CALIBRATION_CORPUS`: the corpus directory (required).
//! - `STENO_MODELS_DIR`: the models directory whose `onnx/diarization/`
//!   holds the ONNX models or receives their download (default: the app's,
//!   `steno_speech::ModelStore::from_environment`), with the mirror
//!   `STENO_MODELS_MIRROR` names.
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
use steno_diarize::{DEFAULT_CLUSTERING_THRESHOLD, DiarizationBackend, DiarizerConfig, Pipeline};

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

fn backend() -> (String, Box<dyn DiarizationBackend>) {
    let kind = std::env::var("STENO_DIARIZE_BACKEND").unwrap_or_else(|_| "onnx".to_owned());
    match kind.as_str() {
        #[cfg(feature = "onnx")]
        "onnx" => {
            let threads = std::env::var("STENO_DIARIZE_THREADS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(4);
            let store = steno_speech::ModelStore::from_environment();
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

/// Every `audio/<id>` in the corpus has a count in `truth`, so an entry
/// `truth.json` holds in a shape [`truth`] cannot read fails the full run
/// rather than leaving that lane out of the gate.
fn assert_every_lane_has_truth(corpus: &Path, truth: &BTreeMap<String, usize>) {
    let missing: Vec<String> = std::fs::read_dir(corpus.join("audio"))
        .expect("audio directory readable")
        .map(|entry| entry.expect("audio entry").file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|id| !id.starts_with('.') && !truth.contains_key(id))
        .collect();
    assert!(
        missing.is_empty(),
        "audio without a count in truth.json: {missing:?}"
    );
}

/// The cuts `STENO_DIARIZE_THRESHOLDS` names, or the default sweep.
fn thresholds() -> Vec<f32> {
    std::env::var("STENO_DIARIZE_THRESHOLDS").ok().map_or_else(
        || vec![0.20, 0.26, 0.32, 0.38, 0.44, 0.50, 0.60],
        |list| {
            list.split(',')
                .map(|s| s.trim().parse().expect("threshold"))
                .collect()
        },
    )
}

/// The backend's heading and the table header with its delimiter row.
fn print_header(name: &str, thresholds: &[f32]) {
    println!("\n### {name}\n");
    println!(
        "Counts per clustering cut (cosine distance); `refined` is after the refinement pass, `mapped` before it.\n"
    );
    println!(
        "| Call | Truth | Minutes | {} | Analysis s | Load |",
        thresholds
            .iter()
            .map(|t| format!("{t:.2} refined / mapped"))
            .collect::<Vec<_>>()
            .join(" | ")
    );
    println!(
        "|---|---:|---:|{}---:|---:|",
        "---:|".repeat(thresholds.len())
    );
}

/// Gate G3 for one lane: exactly the truth for a call with at most one
/// remote speaker, within one of it for a group call.
fn within_gate(refined: usize, expected: usize) -> bool {
    if expected <= 1 {
        refined == expected
    } else {
        refined.abs_diff(expected) <= 1
    }
}

#[test]
fn the_gate_is_exact_for_one_remote_speaker_and_within_one_for_a_group() {
    assert!(within_gate(0, 0));
    assert!(!within_gate(1, 0), "a phantom speaker on a silent lane");
    assert!(within_gate(1, 1));
    assert!(!within_gate(0, 1));
    assert!(!within_gate(2, 1));
    assert!(within_gate(6, 7) && within_gate(7, 7) && within_gate(8, 7));
    assert!(!within_gate(5, 7) && !within_gate(9, 7));
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
    if only.is_none() {
        assert_every_lane_has_truth(&corpus, &truth);
    }
    let thresholds = thresholds();
    let (name, backend) = backend();
    let mut pipeline = Pipeline::new(backend, DiarizerConfig::default());
    print_header(&name, &thresholds);
    let mut gate: BTreeMap<String, Vec<bool>> = BTreeMap::new();
    // Lanes that miss the gate at the default cut or have no audio.
    let mut failures: Vec<String> = Vec::new();
    for (id, expected) in &truth {
        if only
            .as_ref()
            .is_some_and(|list| !list.iter().any(|wanted| is_requested(id, wanted)))
        {
            continue;
        }
        let short = &id[..id.len().min(8)];
        let wav = corpus.join("audio").join(id).join("system.wav");
        if !wav.is_file() {
            eprintln!("{}: missing, counted as a miss", wav.display());
            for threshold in &thresholds {
                gate.entry(format!("{threshold:.2}"))
                    .or_default()
                    .push(false);
            }
            failures.push(format!("{short}: no system.wav"));
            continue;
        }
        let audio = read_wav(&wav);
        let load = load_average();
        let started = Instant::now();
        let analysis = pipeline.analyze(&audio).expect("analysis");
        let analysis_seconds = started.elapsed().as_secs_f64();
        let mut cells = Vec::new();
        let mut at_default = None;
        for threshold in &thresholds {
            let mapped = pipeline.map(&analysis, *threshold);
            let refined = pipeline.refine(&mapped, &audio).expect("refinement");
            let pass = within_gate(refined.clusters.len(), *expected);
            if *threshold == DEFAULT_CLUSTERING_THRESHOLD {
                at_default = Some(refined.clusters.len());
            }
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
            short,
            expected,
            audio.duration() / 60.0,
            cells.join(" | "),
            analysis_seconds,
            load
        );
        // The default cut is asserted even when the sweep leaves it out.
        let refined = at_default.unwrap_or_else(|| {
            let mapped = pipeline.map(&analysis, DEFAULT_CLUSTERING_THRESHOLD);
            let refined = pipeline.refine(&mapped, &audio).expect("refinement");
            refined.clusters.len()
        });
        if !within_gate(refined, *expected) {
            failures.push(format!("{short}: {refined} speakers, truth {expected}"));
        }
    }
    println!();
    for (threshold, results) in &gate {
        let passed = results.iter().filter(|p| **p).count();
        println!(
            "- cut {threshold}: {passed} of {} calls within the gate",
            results.len()
        );
    }
    assert!(
        failures.is_empty(),
        "gate G3 fails at the default cut {DEFAULT_CLUSTERING_THRESHOLD:.2}: {failures:?}"
    );
}
