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
fn the_swift_cli_flow_runs_end_to_end_on_a_fresh_home() {
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
    // The receipt folder is joined onto the vault with the platform's
    // separator, so the printed path is compared piecewise.
    let delivered_folder = deliver
        .stdout
        .strip_prefix(&format!("{ad_hoc}\tdelivered\t"))
        .unwrap_or_else(|| panic!("{}", deliver.stdout));
    assert!(
        delivered_folder.starts_with(&vault.join("Meetings").display().to_string()),
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
fn a_run_whose_meeting_ends_failed_exits_two_and_says_why() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let db = home.join("steno.sqlite");
    // The mic lane is a WAV the command validates up front; the system lane
    // is copied as is and fails in the pipeline's decode stage.
    let not_audio = home.join("system.wav");
    std::fs::write(&not_audio, b"not a wav").unwrap();
    let process = steno(
        &[
            "process",
            fixtures_root().join("audio/sweep-3s.wav").to_str().unwrap(),
            "--source",
            "mac-call",
            "--system-lane",
            not_audio.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
            "--audio-folder",
            home.join("audio").to_str().unwrap(),
        ],
        home,
    );
    assert_eq!(process.status, 2, "{}", process.stderr);
    assert!(
        process.stderr.contains("processing failed: decode: "),
        "{}",
        process.stderr
    );
    assert_eq!(
        process.stderr.matches("processing failed").count(),
        1,
        "one line, Swift's: {}",
        process.stderr
    );
    assert_eq!(process.stdout, "", "no meeting id on a failed run");
}

#[test]
fn relative_paths_are_taken_from_the_working_directory() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let work = home.join("work");
    std::fs::create_dir_all(work.join("nested")).unwrap();
    std::fs::copy(
        fixtures_root().join("audio/sweep-3s.wav"),
        work.join("sweep.wav"),
    )
    .unwrap();
    let run = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_steno"))
            .args(args)
            .current_dir(&work)
            .env("HOME", home)
            .env("XDG_DATA_HOME", home.join("share"))
            .env("APPDATA", home.join("appdata"))
            .env_remove("STENO_LLM_API_KEY")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    };
    let meeting_id = run(&[
        "process",
        "sweep.wav",
        "--db",
        "steno.sqlite",
        "--audio-folder",
        "nested/../audio",
    ]);
    let printed = run(&[
        "export",
        &meeting_id,
        "--out",
        "./out/",
        "--db",
        "steno.sqlite",
    ]);
    let canonical_work = work.canonicalize().unwrap();
    let json_path = PathBuf::from(&printed);
    assert!(json_path.is_absolute(), "{printed}");
    assert_eq!(
        json_path.parent().unwrap().canonicalize().unwrap(),
        canonical_work.join("out")
    );
    assert!(
        std::fs::read_dir(work.join("out"))
            .unwrap()
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().ends_with(".partial")),
        "the temporary was renamed away"
    );
    let exported = export(&json_path);
    let url = exported["audio"]["url"].as_str().unwrap();
    let stored = path_of_file_url(url);
    assert!(stored.is_absolute(), "{url}");
    assert!(
        !url.contains("/../") && !url.contains("nested"),
        "the audio folder was standardized: {url}"
    );
    assert!(
        stored.starts_with(&work) || stored.starts_with(&canonical_work),
        "{url}"
    );
}

/// The path of a `file://` URL as the export writes it.
fn path_of_file_url(url: &str) -> PathBuf {
    let rest = url.strip_prefix("file://").unwrap();
    // `file:///C:/...` on Windows keeps a slash before the drive letter.
    let rest = if cfg!(windows) {
        rest.trim_start_matches('/')
    } else {
        rest
    };
    PathBuf::from(rest.replace("%20", " "))
}

/// `STENO_MODELS_DIR` names the models directory, taken from the working
/// directory when relative, as the app and the `transcribe` example take it
/// (`crates/steno-services/tests/models_directory.rs` pins those two).
#[test]
fn the_models_variable_names_the_models_directory() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let work = home.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let relative = Path::new("relative").join("models");
    let expected = work.join(&relative);
    std::fs::create_dir_all(&expected).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_steno"))
        .args(["dev", "models", "list", "--db"])
        .arg(home.join("steno.sqlite"))
        .current_dir(&work)
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join("share"))
        .env("APPDATA", home.join("appdata"))
        .env("STENO_MODELS_DIR", &relative)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{output:?}");
    let printed = stdout
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("models: "))
        .unwrap_or_else(|| panic!("{stdout}"));
    // Compared as the file system sees them: the working directory may come
    // back resolved (`/private/var` on the Mac, a long name on Windows).
    assert_eq!(
        Path::new(printed).canonicalize().unwrap(),
        expected.canonicalize().unwrap(),
        "{stdout}"
    );
}

/// `steno dev models <args> --models-dir <home>/models` with `home`'s
/// support directory, which must succeed.
fn dev_models(home: &Path, args: &[&str]) -> Run {
    let models = home.join("models");
    let mut command = vec!["dev", "models"];
    command.extend_from_slice(args);
    command.extend(["--models-dir", models.to_str().unwrap()]);
    let run = steno(&command, home);
    assert_eq!(run.status, 0, "{args:?}: {}", run.stderr);
    run
}

/// `dev models list` and `remove` act on the `--models-dir` they are given,
/// not on the default models directory.
#[test]
fn dev_models_lists_and_removes_in_the_models_dir_it_is_given() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let models = home.join("models");

    // The diarizer's two files in `--models-dir`.
    let diarization = models.join("onnx").join("diarization");
    std::fs::create_dir_all(&diarization).unwrap();
    let diarizer_files = [
        diarization.join(steno_diarize::models::PYANNOTE_SEGMENTATION_3_0.file_name),
        diarization.join(steno_diarize::models::WESPEAKER_RESNET34_LM.file_name),
    ];
    for file in &diarizer_files {
        std::fs::write(file, b"onnx").unwrap();
    }
    let list = dev_models(home, &["list"]);
    assert!(
        list.stdout
            .contains(&format!("models: {}", models.display())),
        "{}",
        list.stdout
    );
    assert_eq!(
        list.stdout.matches("not installed (~").count(),
        4,
        "four absent assets: {}",
        list.stdout
    );
    assert!(
        list.stdout
            .lines()
            .any(|line| line.starts_with("offlineDiarizer ") && line.contains(": installed (")),
        "the diarizer in --models-dir: {}",
        list.stdout
    );
    dev_models(home, &["remove", "offlineDiarizer"]);
    assert!(
        diarizer_files.iter().all(|file| !file.exists()),
        "removed from --models-dir"
    );
    // Removing an asset that is not installed succeeds.
    dev_models(home, &["remove", "parakeetV3"]);
}

/// The line of `asset` in `steno dev models list` over [`dev_models`].
fn models_list_line(home: &Path, asset: &str) -> String {
    let list = dev_models(home, &["list"]);
    list.stdout
        .lines()
        .find(|line| line.starts_with(&format!("{asset} ")))
        .unwrap_or_else(|| panic!("{}", list.stdout))
        .to_owned()
}

/// Parakeet v3 is listed with the size of the model the platform runs:
/// the `CoreML` build's on the Mac by default; the fp32 export's (about
/// 2.6 GB) elsewhere, and on the Mac too once `speech.json` chooses the
/// speech sidecar.
#[test]
fn dev_models_list_shows_the_size_of_the_parakeet_the_platform_runs() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let not_installed =
        |bytes: i64| format!("not installed (~{})", steno_host::labels::file_size(bytes));
    let coreml = not_installed(steno_host::speech::ModelAsset::ParakeetV3.approximate_bytes());
    let fp32 = not_installed(
        i64::try_from(steno_speech::ModelAsset::parakeet_v3_fp32().total_size()).unwrap(),
    );
    assert_ne!(coreml, fp32);

    let line = models_list_line(home, "parakeetV3");
    let expected = if cfg!(target_os = "macos") {
        &coreml
    } else {
        &fp32
    };
    assert!(line.ends_with(expected.as_str()), "{line}");

    let environment = [
        ("HOME", home.to_path_buf()),
        ("XDG_DATA_HOME", home.join("share")),
        ("APPDATA", home.join("appdata")),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_string_lossy().into_owned()))
    .collect();
    let support = steno_core::StenoPaths::support_directory(&environment);
    std::fs::create_dir_all(&support).unwrap();
    std::fs::write(
        support.join("speech.json"),
        br#"{"onnxSidecarOnMac": true}"#,
    )
    .unwrap();
    let line = models_list_line(home, "parakeetV3");
    assert!(line.ends_with(fp32.as_str()), "the sidecar chosen: {line}");
}

// Every usage error of the Swift test in one place.
#[allow(clippy::too_many_lines)]
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
    // Both are in `StenoJSON`'s form, as the Swift bake-off wrote them:
    // pretty, sorted keys, ` : ` between a key and its value.
    for name in ["report.json", "tone-3s.parakeet-v3.json"] {
        let text = std::fs::read_to_string(out.join(name)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            text,
            steno_core::json::to_canonical_string(&value).unwrap(),
            "{name}"
        );
    }
}
