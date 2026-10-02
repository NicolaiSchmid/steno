//! The `steno` binary end to end on a temporary home: migrate, generate
//! the fixtures, process, export, reindex, deliver.
//! Swift: `Tests/stenoTests/CLITests.swift`.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Run {
    status: i32,
    stdout: String,
    stderr: String,
}

fn steno(args: &[&str], home: &Path) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_steno"))
        .args(args)
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join("share"))
        .env("APPDATA", home.join("appdata"))
        .env_remove("STENO_LLM_API_KEY")
        .output()
        .expect("the steno binary runs");
    Run {
        status: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Tests/Fixtures")
}

fn export(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

// The Swift test is one flow too: each step reads the one before.
#[allow(clippy::too_many_lines)]
#[test]
fn migrate_generate_process_export_and_deliver() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let db = home.join("db/steno.sqlite");
    let db = db.to_str().unwrap();

    let migrate = steno(&["dev", "db", "migrate", "--db", db], home);
    assert_eq!(migrate.status, 0, "{}", migrate.stderr);
    assert!(migrate.stdout.contains("v1"), "{}", migrate.stdout);
    assert!(Path::new(db).exists());

    let fixtures = home.join("fixtures");
    let generate = steno(
        &[
            "dev",
            "fixtures",
            "generate",
            "--out",
            fixtures.to_str().unwrap(),
        ],
        home,
    );
    assert_eq!(generate.status, 0, "{}", generate.stderr);
    assert!(generate.stdout.contains("audio/sweep-3s.wav"));
    assert!(fixtures.join("MANIFEST.sha256").exists());
    assert_eq!(
        std::fs::read(fixtures.join("audio/sweep-3s.wav")).unwrap(),
        std::fs::read(fixtures_root().join("audio/sweep-3s.wav")).unwrap(),
        "the generator is byte-identical to the committed fixture"
    );
    assert_eq!(
        std::fs::read_to_string(fixtures.join("MANIFEST.sha256")).unwrap(),
        std::fs::read_to_string(fixtures_root().join("MANIFEST.sha256")).unwrap(),
        "and so is the manifest"
    );

    let audio = home.join("audio");
    let process = steno(
        &[
            "process",
            fixtures.join("audio/sweep-3s.wav").to_str().unwrap(),
            "--source",
            "mac-in-person",
            "--title",
            "Sweep",
            "--db",
            db,
            "--audio-folder",
            audio.to_str().unwrap(),
        ],
        home,
    );
    assert_eq!(process.status, 0, "{}", process.stderr);
    assert!(
        process
            .stderr
            .contains("summary skipped: no LLM endpoint configured"),
        "{}",
        process.stderr
    );
    assert!(
        process.stderr.contains("transcribe"),
        "progress lines go to stderr: {}",
        process.stderr
    );
    let meeting_id = process.stdout.trim().to_owned();
    assert!(
        uuid::Uuid::parse_str(&meeting_id).is_ok(),
        "stdout carries the meeting id and nothing else: {}",
        process.stdout
    );
    let folder = audio.join(meeting_id.to_uppercase());
    let folder = if folder.exists() {
        folder
    } else {
        audio.join(&meeting_id)
    };
    assert!(
        folder.join("recording.wav").exists(),
        "{}",
        folder.display()
    );
    assert!(folder.join("audio.wav").exists(), "the mixdown");
    assert_eq!(
        std::fs::read_dir(folder.join("speakers")).unwrap().count(),
        2,
        "two sample clips from the fake diarizer"
    );

    let out = home.join("out");
    let exported = steno(
        &[
            "export",
            &meeting_id,
            "--out",
            out.to_str().unwrap(),
            "--db",
            db,
        ],
        home,
    );
    assert_eq!(exported.status, 0, "{}", exported.stderr);
    let decoded = export(&out.join("meeting.json"));
    assert_eq!(
        decoded["meeting"]["id"].as_str().unwrap().to_lowercase(),
        meeting_id.to_lowercase()
    );
    assert_eq!(decoded["meeting"]["state"], "ready");
    assert_eq!(decoded["meeting"]["source"], "macInPerson");
    assert_eq!(
        decoded["meeting"]["title"], "Sweep",
        "no fake summarizer renames the meeting"
    );
    assert!(decoded["meeting"].get("summary").is_none() || decoded["meeting"]["summary"].is_null());
    assert_eq!(decoded["segments"].as_array().unwrap().len(), 3);
    assert!(decoded["audio"]["mixdownURL"].is_string());

    let reindex = steno(&["dev", "db", "reindex", "--db", db], home);
    assert_eq!(reindex.status, 0, "{}", reindex.stderr);

    let call = steno(
        &[
            "process",
            fixtures
                .join("audio/conversation-mic-6s.wav")
                .to_str()
                .unwrap(),
            "--system-lane",
            fixtures
                .join("audio/conversation-system-6s.wav")
                .to_str()
                .unwrap(),
            "--source",
            "mac-call",
            "--template",
            "daily-standup",
            "--db",
            db,
            "--audio-folder",
            audio.to_str().unwrap(),
        ],
        home,
    );
    assert_eq!(call.status, 0, "{}", call.stderr);
    let call_id = call.stdout.trim().to_owned();
    let call_out = out.join("call");
    let call_export = steno(
        &[
            "export",
            &call_id,
            "--out",
            call_out.to_str().unwrap(),
            "--db",
            db,
        ],
        home,
    );
    assert_eq!(call_export.status, 0, "{}", call_export.stderr);
    let call_decoded = export(&call_out.join("meeting.json"));
    let labels: Vec<&str> = call_decoded["speakers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["clusterLabel"].as_str().unwrap())
        .collect();
    assert_eq!(labels, ["Me", "Speaker 1", "Speaker 2"]);
    assert_eq!(call_decoded["meeting"]["templateID"], "daily-standup");

    // deliver: --vault builds the destination for this run only.
    let vault = home.join("vault");
    std::fs::create_dir_all(&vault).unwrap();
    let deliver = steno(
        &[
            "deliver",
            &meeting_id,
            "--vault",
            vault.to_str().unwrap(),
            "--people-folder",
            "People",
            "--include-audio",
            "--task-tag",
            "task",
            "--db",
            db,
        ],
        home,
    );
    assert_eq!(deliver.status, 0, "{}", deliver.stderr);
    let ad_hoc = format!("obsidian-folder@{}", vault.display());
    assert!(
        deliver.stdout.starts_with(&format!(
            "{ad_hoc}\tdelivered\t{}/Meetings/",
            vault.display()
        )),
        "{}",
        deliver.stdout
    );
    let meetings = vault.join("Meetings");
    let folders: Vec<_> = std::fs::read_dir(&meetings).unwrap().flatten().collect();
    assert_eq!(folders.len(), 1);
    let slug = folders[0].file_name().to_string_lossy().into_owned();
    assert!(
        slug.ends_with("-sweep"),
        "named after the title, not a fake summary: {slug}"
    );
    let mut files: Vec<String> = std::fs::read_dir(meetings.join(&slug))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            format!("{slug} - Tasks.md"),
            format!("{slug} - Transcript.md"),
            format!("{slug}.md"),
            "audio.wav".to_owned(),
            "meeting.json".to_owned(),
            "transcript.vtt".to_owned(),
        ],
        "six files: the WAV decoder's mixdown is copied as audio.wav"
    );
    let note = std::fs::read_to_string(meetings.join(&slug).join(format!("{slug}.md"))).unwrap();
    assert!(
        note.contains("No summary."),
        "a skipped summary is said, not left blank: {note}"
    );

    // Without --vault the stored settings decide.
    let unconfigured = steno(&["deliver", &meeting_id, "--db", db], home);
    assert_eq!(unconfigured.status, 2);
    assert!(
        unconfigured.stderr.contains("No destination configured"),
        "{}",
        unconfigured.stderr
    );

    let flags_without_vault = steno(
        &["deliver", &meeting_id, "--include-audio", "--db", db],
        home,
    );
    assert_eq!(flags_without_vault.status, 1);
    assert_eq!(steno(&["deliver", "nope", "--db", db], home).status, 1);
}

#[test]
fn usage_errors_exit_one_and_name_the_known_values() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let db = home.join("steno.sqlite");
    let db = db.to_str().unwrap();
    let models = home.join("models");

    let bad_engine = steno(
        &[
            "process",
            "/nonexistent.wav",
            "--engine",
            "parakeet-v9",
            "--db",
            db,
        ],
        home,
    );
    assert_eq!(bad_engine.status, 1);
    assert!(
        bad_engine.stderr.contains("parakeet-v9"),
        "{}",
        bad_engine.stderr
    );
    assert!(
        bad_engine.stderr.contains("parakeet-v3"),
        "the known ids are listed: {}",
        bad_engine.stderr
    );
    assert!(bad_engine.stderr.contains("whisperkit-large-v3-turbo"));

    let help = steno(&["process", "--help"], home);
    assert_eq!(help.status, 0);
    assert!(help.stdout.contains("--engine <engine>"), "{}", help.stdout);

    let bad_bakeoff = steno(
        &[
            "dev",
            "bakeoff",
            home.to_str().unwrap(),
            "--engines",
            "nope",
        ],
        home,
    );
    assert_eq!(bad_bakeoff.status, 1);
    assert!(bad_bakeoff.stderr.contains("nope") && bad_bakeoff.stderr.contains("parakeet-v3"));

    let list = steno(
        &[
            "dev",
            "models",
            "list",
            "--models-dir",
            models.to_str().unwrap(),
        ],
        home,
    );
    assert_eq!(list.status, 0, "{}", list.stderr);
    assert!(
        list.stdout
            .contains(&format!("models: {}", models.display())),
        "{}",
        list.stdout
    );
    assert_eq!(
        list.stdout.matches("not installed (~").count(),
        5,
        "five absent assets: {}",
        list.stdout
    );

    let remove = steno(
        &[
            "dev",
            "models",
            "remove",
            "parakeetV3",
            "--models-dir",
            models.to_str().unwrap(),
        ],
        home,
    );
    assert_eq!(remove.status, 0, "{}", remove.stderr);
    let bad_asset = steno(
        &[
            "dev",
            "models",
            "remove",
            "nope",
            "--models-dir",
            models.to_str().unwrap(),
        ],
        home,
    );
    assert_eq!(bad_asset.status, 1);
    assert!(
        bad_asset.stderr.contains("offlineDiarizer"),
        "the known assets are listed: {}",
        bad_asset.stderr
    );

    let no_inputs = steno(&["dev", "aec-bench"], home);
    assert_eq!(no_inputs.status, 1);
    assert!(no_inputs.stderr.contains("--synthetic"));
    assert_eq!(
        steno(
            &["dev", "aec-bench", "--synthetic", "--engine", "webrtc"],
            home
        )
        .status,
        1
    );
    assert_eq!(
        steno(
            &[
                "dev",
                "capture-spike",
                "--lanes",
                "phone",
                "--out",
                home.to_str().unwrap()
            ],
            home
        )
        .status,
        1
    );
    let zero = steno(
        &[
            "record",
            "--backend",
            "synthetic",
            "--seconds",
            "0",
            "--out",
            home.to_str().unwrap(),
        ],
        home,
    );
    assert_eq!(zero.status, 1);
    assert!(zero.stderr.contains("positive"));
    assert_eq!(
        steno(
            &[
                "record",
                "--backend",
                "tape",
                "--out",
                home.to_str().unwrap()
            ],
            home
        )
        .status,
        1
    );
    assert_eq!(
        steno(
            &["record", "--mode", "phone", "--out", home.to_str().unwrap()],
            home
        )
        .status,
        1
    );

    let unconfigured = steno(&["dev", "llm", "probe", "--db", db], home);
    assert_eq!(unconfigured.status, 1);
    assert!(
        unconfigured.stderr.contains("No LLM endpoint configured"),
        "{}",
        unconfigured.stderr
    );
}

#[test]
fn aec_bench_runs_on_the_synthetic_fixtures() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let passthrough = steno(
        &["dev", "aec-bench", "--synthetic", "--engine", "passthrough"],
        home,
    );
    assert_eq!(passthrough.status, 0, "{}", passthrough.stderr);
    assert!(
        passthrough
            .stdout
            .contains("engine: passthrough, tail 200 ms"),
        "{}",
        passthrough.stdout
    );
    assert!(
        passthrough.stdout.contains("after 3 s 0.0 dB"),
        "{}",
        passthrough.stdout
    );
    let out = home.join("processed.wav");
    let speex = steno(
        &[
            "dev",
            "aec-bench",
            "--synthetic",
            "--tail-milliseconds",
            "100",
            "--out",
            out.to_str().unwrap(),
        ],
        home,
    );
    assert_eq!(speex.status, 0, "{}", speex.stderr);
    assert!(
        speex.stdout.contains("engine: speex, tail 100 ms"),
        "{}",
        speex.stdout
    );
    let steady: f64 = speex
        .stdout
        .lines()
        .find_map(|line| line.strip_prefix("after 3 s "))
        .and_then(|rest| rest.trim_end_matches(" dB").parse().ok())
        .expect("an ERLE line");
    assert!(steady >= 20.0, "ERLE after 3 s: {steady} dB");
    assert!(out.exists());
}

#[test]
fn record_with_the_synthetic_backend_writes_a_meeting_folder() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let out = home.join("audio");
    let id = uuid::Uuid::new_v4().to_string();
    let result = steno(
        &[
            "record",
            "--backend",
            "synthetic",
            "--mode",
            "in-person",
            "--seconds",
            "0.5",
            "--out",
            out.to_str().unwrap(),
            "--meeting-id",
            &id,
            "--quiet",
        ],
        home,
    );
    assert_eq!(result.status, 0, "{}", result.stderr);
    assert!(
        result
            .stdout
            .contains(&format!("meeting: {}", id.to_uppercase()))
            || result.stdout.contains(&format!("meeting: {id}")),
        "{}",
        result.stdout
    );
    assert!(
        result.stdout.contains("dropped frames: none"),
        "{}",
        result.stdout
    );
    assert!(result.stdout.contains("device changes: 0"));
    assert!(result.stdout.contains("gap filled: 0.00 s"));
}

#[test]
fn bakeoff_with_fake_engines_reports_one_segment_per_second() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let audio = home.join("bakeoff-in");
    std::fs::create_dir_all(&audio).unwrap();
    std::fs::copy(
        fixtures_root().join("audio/sweep-3s.wav"),
        audio.join("tone-3s.wav"),
    )
    .unwrap();
    std::fs::write(
        audio.join("tone-3s.txt"),
        "fake segment 1 fake segment 2 fake segment 3\n",
    )
    .unwrap();
    let out = home.join("reports");
    let result = steno(
        &[
            "dev",
            "bakeoff",
            audio.to_str().unwrap(),
            "--fake-engines",
            "--engines",
            "parakeet-v3,whisperkit-large-v3-turbo",
            "--out",
            out.to_str().unwrap(),
            "--db",
            home.join("steno.sqlite").to_str().unwrap(),
        ],
        home,
    );
    assert_eq!(result.status, 0, "{}", result.stderr);
    assert!(
        result
            .stdout
            .contains("| tone-3s.wav | parakeet-v3 | 3.00 |"),
        "{}",
        result.stdout
    );
    assert!(
        result
            .stdout
            .contains("| tone-3s.wav | whisperkit-large-v3-turbo | 3.00 |")
    );
    assert!(
        result.stdout.contains("| 3 | 0.0 %"),
        "WER against the exact reference: {}",
        result.stdout
    );
    assert!(
        result
            .stdout
            .ends_with(&format!("reports: {}\n", out.display()))
    );
    let written: Vec<String> = std::fs::read_dir(&out)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        written.contains(&"report.json".to_owned()) && written.contains(&"report.md".to_owned())
    );
    assert!(written.contains(&"tone-3s.parakeet-v3.json".to_owned()));
}
