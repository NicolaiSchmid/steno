//! The `steno` binary end to end on a temporary home: migrate, generate
//! the fixtures, process, export, reindex, deliver.
//! Swift: `Tests/stenoTests/CLITests.swift`.

use std::path::{Path, PathBuf};
use std::process::Command;

use steno_core::testing::{database_one_version_behind, recorded_migrations};

struct Run {
    status: i32,
    stdout: String,
    stderr: String,
}

fn steno(args: &[&str], home: &Path) -> Run {
    let mut command = Command::new(env!("CARGO_BIN_EXE_steno"));
    command
        .args(args)
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join("share"))
        .env("APPDATA", home.join("appdata"))
        .env_remove("STENO_LLM_API_KEY");
    // Every run on Unix carries a variable that is not Unicode, as a
    // user's environment may: the secret overrides, which read every
    // variable, must read past it (`std::env::vars()` would panic).
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        command.env(
            "STENO_TEST_NOT_UNICODE",
            std::ffi::OsStr::from_bytes(&[b'a', 0x80]),
        );
    }
    let output = command.output().expect("the steno binary runs");
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
        decoded["meeting"]["titleOrigin"], "user",
        "--title is the user's title, so the app shows it as the export does"
    );
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

/// A command that writes refuses while another process (the app) holds the
/// database's lock, and runs once it is released; one that only reads runs
/// beside it.
#[test]
fn a_writing_command_refuses_while_the_app_holds_the_database() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let db = home.join("steno.sqlite");
    let db_arg = db.to_str().unwrap();
    let migrate = ["dev", "db", "migrate", "--db", db_arg];
    assert_eq!(steno(&migrate, home).status, 0);
    let wav = fixtures_root().join("audio/sweep-3s.wav");
    let audio = home.join("audio");

    let app = steno_core::DatabaseLock::acquire(&db).unwrap();
    for args in [
        &migrate[..],
        &["dev", "db", "reindex", "--db", db_arg][..],
        &[
            "deliver",
            "00000000-0000-0000-0000-000000000001",
            "--db",
            db_arg,
        ][..],
        &[
            "process",
            wav.to_str().unwrap(),
            "--db",
            db_arg,
            "--audio-folder",
            audio.to_str().unwrap(),
        ][..],
    ] {
        let refused = steno(args, home);
        assert_eq!(refused.status, 2, "{args:?}: {}", refused.stderr);
        assert!(
            refused.stderr.contains(&format!(
                "Steno, or another steno command, is using {db_arg}; quit it first"
            )),
            "{args:?}: {}",
            refused.stderr
        );
    }
    assert!(!audio.exists(), "the refused process copied nothing");
    let read = export_beside(&db, home);
    assert!(!read.stderr.contains("quit it first"), "{}", read.stderr);

    drop(app);
    let migrated = steno(&migrate, home);
    assert_eq!(migrated.status, 0, "{}", migrated.stderr);
}

/// `steno export` of a meeting that is not there, on `db`.
fn export_beside(db: &Path, home: &Path) -> Run {
    steno(
        &[
            "export",
            "00000000-0000-0000-0000-000000000001",
            "--db",
            db.to_str().unwrap(),
            "--out",
            home.join("out").to_str().unwrap(),
        ],
        home,
    )
}

/// A command that only reads, beside the app, opens the database without
/// migrating it: it runs on a database at this build's version and changes
/// nothing, and refuses one an older app still runs on (or the app is
/// still migrating) rather than migrate the schema under it.
#[test]
fn a_reading_command_beside_the_app_never_migrates() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let current = home.join("current.sqlite");
    let migrate = steno(
        &["dev", "db", "migrate", "--db", current.to_str().unwrap()],
        home,
    );
    assert_eq!(migrate.status, 0, "{}", migrate.stderr);
    let before = recorded_migrations(&current);
    let _app = steno_core::DatabaseLock::acquire(&current).unwrap();
    let read = export_beside(&current, home);
    assert!(
        read.stderr.contains("not found"),
        "it opened the database and looked the meeting up: {}",
        read.stderr
    );
    assert_eq!(recorded_migrations(&current), before);

    let older = home.join("older.sqlite");
    database_one_version_behind(&older);
    let before = recorded_migrations(&older);
    let _older_app = steno_core::DatabaseLock::acquire(&older).unwrap();
    let refused = export_beside(&older, home);
    assert_eq!(refused.status, 2, "{}", refused.stderr);
    assert!(
        refused.stderr.contains(
            "Steno is updating its database, or an older Steno is running; quit it first, then run this command again."
        ),
        "{}",
        refused.stderr
    );
    assert_eq!(recorded_migrations(&older), before, "nothing was migrated");
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

    // The diarizer's two files in `--models-dir`, each a sparse file of
    // its manifest size.
    let diarization = models.join("onnx").join("diarization");
    std::fs::create_dir_all(&diarization).unwrap();
    for file in &steno_diarize::models::asset().files {
        std::fs::File::create(diarization.join(&file.name))
            .unwrap()
            .set_len(file.size)
            .unwrap();
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
    assert!(!diarization.exists(), "removed from --models-dir");
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

    let support = steno_core::StenoPaths::support_directory(|name| match name {
        "HOME" => Some(home.into()),
        "XDG_DATA_HOME" => Some(home.join("share").into()),
        "APPDATA" => Some(home.join("appdata").into()),
        _ => None,
    });
    std::fs::create_dir_all(&support).unwrap();
    std::fs::write(
        support.join("speech.json"),
        br#"{"onnxSidecarOnMac": true}"#,
    )
    .unwrap();
    let line = models_list_line(home, "parakeetV3");
    assert!(line.ends_with(fp32.as_str()), "the sidecar chosen: {line}");
}

/// `steno process --meeting <id>` processes a stored failed meeting again
/// from its recording: refused while the master is gone, and once the
/// broken lane is replaced the run ends ready and prints the id. A ready
/// meeting is refused unless `--allow-ready` asks for it, its master on
/// disk or not.
#[allow(clippy::too_many_lines)]
#[test]
fn process_meeting_runs_a_failed_meeting_again_from_its_recording() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let db = home.join("steno.sqlite");
    let db = db.to_str().unwrap();
    let audio = home.join("audio");
    let broken = home.join("system.wav");
    std::fs::write(&broken, b"not a wav").unwrap();
    let first = steno(
        &[
            "process",
            fixtures_root()
                .join("audio/conversation-mic-6s.wav")
                .to_str()
                .unwrap(),
            "--source",
            "mac-call",
            "--system-lane",
            broken.to_str().unwrap(),
            "--db",
            db,
            "--audio-folder",
            audio.to_str().unwrap(),
        ],
        home,
    );
    assert_eq!(first.status, 2, "{}", first.stderr);
    let folders: Vec<PathBuf> = std::fs::read_dir(&audio)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    let [folder] = folders.as_slice() else {
        panic!("one meeting folder: {folders:?}");
    };
    let meeting_id = folder.file_name().unwrap().to_str().unwrap().to_owned();

    let master = folder.join("mic.wav");
    let aside = home.join("mic-aside.wav");
    std::fs::rename(&master, &aside).unwrap();
    let gone = steno(&["process", "--meeting", &meeting_id, "--db", db], home);
    assert_eq!(gone.status, 2, "{}", gone.stderr);
    assert!(
        gone.stderr
            .contains("is no longer on disk, so it cannot be processed again."),
        "{}",
        gone.stderr
    );
    std::fs::rename(&aside, &master).unwrap();

    std::fs::copy(
        fixtures_root().join("audio/conversation-system-6s.wav"),
        folder.join("system.wav"),
    )
    .unwrap();
    let again = steno(&["process", "--meeting", &meeting_id, "--db", db], home);
    assert_eq!(again.status, 0, "{}", again.stderr);
    assert!(again.stderr.contains("transcribe"), "{}", again.stderr);
    assert_eq!(
        again.stdout.trim().to_lowercase(),
        meeting_id.to_lowercase(),
        "stdout carries the meeting id"
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
    assert_eq!(
        export(&out.join("meeting.json"))["meeting"]["state"],
        "ready"
    );

    let ready = steno(&["process", "--meeting", &meeting_id, "--db", db], home);
    assert_eq!(ready.status, 2, "{}", ready.stderr);
    assert!(
        ready.stderr.contains(&format!(
            "Meeting {meeting_id} is ready; pass --allow-ready to process it again."
        )),
        "{}",
        ready.stderr
    );
    assert_eq!(ready.stdout, "");
    // The rule comes before the files: a ready meeting whose master is
    // gone still asks for --allow-ready.
    std::fs::rename(&master, &aside).unwrap();
    let swept = steno(&["process", "--meeting", &meeting_id, "--db", db], home);
    assert_eq!(swept.status, 2, "{}", swept.stderr);
    assert!(
        swept.stderr.contains(&format!(
            "Meeting {meeting_id} is ready; pass --allow-ready to process it again."
        )),
        "{}",
        swept.stderr
    );
    std::fs::rename(&aside, &master).unwrap();
    let allowed = steno(
        &[
            "process",
            "--meeting",
            &meeting_id,
            "--allow-ready",
            "--db",
            db,
        ],
        home,
    );
    assert_eq!(
        allowed.status, 0,
        "--allow-ready runs a ready meeting again: {}",
        allowed.stderr
    );
    assert!(allowed.stderr.contains("transcribe"), "{}", allowed.stderr);
}

/// `--meeting` stands instead of the input, `--allow-ready` needs it, and
/// an id no meeting has is a usage error that says so.
#[test]
fn process_meeting_is_exclusive_of_the_input_and_names_an_unknown_id() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let db = home.join("steno.sqlite");
    let db = db.to_str().unwrap();
    let sweep = fixtures_root().join("audio/sweep-3s.wav");
    let sweep = sweep.to_str().unwrap();
    let id = "6F9619FF-8B86-D011-B42D-00C04FC964FF";
    let both = steno(&["process", sweep, "--meeting", id, "--db", db], home);
    assert_eq!(both.status, 1, "{}", both.stderr);
    assert!(
        both.stderr.contains("cannot be used with"),
        "{}",
        both.stderr
    );
    // `--allow-ready` needs `--meeting`, beside an input or alone.
    let beside = steno(&["process", sweep, "--allow-ready", "--db", db], home);
    assert_eq!(beside.status, 1, "{}", beside.stderr);
    assert!(
        beside.stderr.contains("--allow-ready") && beside.stderr.contains("cannot be used with"),
        "{}",
        beside.stderr
    );
    let alone = steno(&["process", "--allow-ready", "--db", db], home);
    assert_eq!(alone.status, 1, "{}", alone.stderr);
    assert!(alone.stderr.contains("--meeting <ID>"), "{}", alone.stderr);
    let neither = steno(&["process", "--db", db], home);
    assert_eq!(neither.status, 1, "{}", neither.stderr);
    assert!(neither.stderr.contains("<INPUT>"), "{}", neither.stderr);
    let not_an_id = steno(&["process", "--meeting", "nope", "--db", db], home);
    assert_eq!(not_an_id.status, 1, "{}", not_an_id.stderr);
    assert!(
        not_an_id.stderr.contains("nope is not a UUID."),
        "{}",
        not_an_id.stderr
    );

    let unknown = steno(&["process", "--meeting", id, "--db", db], home);
    assert_eq!(unknown.status, 1, "{}", unknown.stderr);
    assert!(
        unknown
            .stderr
            .contains(&format!("No meeting has the id {id}.")),
        "{}",
        unknown.stderr
    );
    assert_eq!(unknown.stdout, "");
}

/// `--meeting` refuses every flag of a new meeting, each by name, and
/// checks the speech flags before it opens the database.
#[test]
fn process_meeting_refuses_each_new_meeting_flag() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let db = home.join("steno.sqlite");
    let db = db.to_str().unwrap();
    let lane = fixtures_root().join("audio/conversation-system-6s.wav");
    let lane = lane.to_str().unwrap();
    let folder = home.join("audio");
    let folder = folder.to_str().unwrap();
    let id = "6F9619FF-8B86-D011-B42D-00C04FC964FF";
    for (flag, value) in [
        ("--title", "Sweep"),
        ("--source", "phone"),
        ("--system-lane", lane),
        ("--template", "default"),
        ("--audio-folder", folder),
    ] {
        let run = steno(&["process", "--meeting", id, flag, value, "--db", db], home);
        assert_eq!(run.status, 1, "{flag}: {}", run.stderr);
        assert!(
            run.stderr.contains("cannot be used with") && run.stderr.contains(flag),
            "{flag}: {}",
            run.stderr
        );
        assert_eq!(run.stdout, "", "{flag}");
    }
    // An unknown engine is the usage error, before the database is opened.
    let engine = steno(
        &[
            "process",
            "--meeting",
            id,
            "--engine",
            "parakeet-v9",
            "--db",
            db,
        ],
        home,
    );
    assert_eq!(engine.status, 1, "{}", engine.stderr);
    assert!(engine.stderr.contains("parakeet-v9"), "{}", engine.stderr);
    assert!(
        !engine.stderr.contains("No meeting has the id"),
        "{}",
        engine.stderr
    );
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

/// `dev onsets` prints where a sound starts in each channel, from
/// `--after` on, in a two-channel 16 kHz WAV (a call master's channel 1 is
/// the system lane): on the first a click at 0.25 s and a tone from 1.5 s,
/// on the second a click at 0.5 s and a quieter tone from 1.25 s.
#[test]
fn dev_onsets_finds_the_first_loud_sample_of_each_channel() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let path = home.join("tone.wav");
    let channel = |click: usize, tone: usize, level: i16| {
        let mut samples = vec![0i16; 32_000];
        samples[click] = 16_000;
        for (index, sample) in samples[tone..].iter_mut().enumerate() {
            *sample = if index % 16 < 8 { level } else { -level };
        }
        samples
    };
    let (first, second) = (channel(4_000, 24_000, 8_000), channel(8_000, 20_000, 4_000));
    // A 16-bit PCM WAV: `RIFF`, `fmt ` (two channels at 16 kHz), `data`.
    let data: Vec<u8> = first
        .iter()
        .zip(&second)
        .flat_map(|(a, b)| [a.to_le_bytes(), b.to_le_bytes()].concat())
        .collect();
    let size = u32::try_from(data.len()).unwrap();
    let mut wav = b"RIFF".to_vec();
    wav.extend_from_slice(&(36 + size).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    for field in [16u32, 0x0002_0001, 16_000, 64_000, 0x0010_0004] {
        wav.extend_from_slice(&field.to_le_bytes());
    }
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&size.to_le_bytes());
    wav.extend_from_slice(&data);
    std::fs::write(&path, wav).unwrap();
    let file = path.to_str().unwrap();

    let from_start = steno(&["dev", "onsets", file], home);
    assert_eq!(from_start.status, 0, "{}", from_start.stderr);
    assert_eq!(
        from_start.stdout.trim(),
        format!(
            "{file} channel 0: onset 0.250 s, peak -6.2 dBFS\n\
             {file} channel 1: onset 0.500 s, peak -6.2 dBFS"
        )
    );
    let after = steno(&["dev", "onsets", "--after", "1", file], home);
    assert_eq!(
        after.stdout.trim(),
        format!(
            "{file} channel 0: onset 1.500 s, peak -12.2 dBFS\n\
             {file} channel 1: onset 1.250 s, peak -18.3 dBFS"
        )
    );
    let quiet = steno(&["dev", "onsets", "--threshold", "-3", file], home);
    assert!(quiet.stdout.contains("no onset"), "{}", quiet.stdout);
    let wrong = steno(&["dev", "onsets", "--after", "-1", file], home);
    assert_eq!(wrong.status, 1);
    let missing = steno(&["dev", "onsets", "missing.wav"], home);
    assert_eq!(missing.status, 2, "{}", missing.stdout);
}

/// A call recorded with `--keep-raw-mic` keeps the microphone before echo
/// cancellation as `mic.raw.caf` beside the master, and says where: the
/// second take of the stable plan's A10 step 2 on the Mac reads the tone's
/// residual there when the cancellation hides it.
#[test]
fn record_keeps_the_raw_mic_when_asked() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let out = home.join("audio");
    let id = uuid::Uuid::new_v4();
    let result = steno(
        &[
            "record",
            "--backend",
            "synthetic",
            "--mode",
            "call",
            "--seconds",
            "0.5",
            "--keep-raw-mic",
            "--out",
            out.to_str().unwrap(),
            "--meeting-id",
            &id.to_string(),
            "--quiet",
        ],
        home,
    );
    assert_eq!(result.status, 0, "{}", result.stderr);
    let layout = steno_core::RecordingLayout::new(&out, id);
    let raw = layout.directory.join("mic.raw.caf");
    assert!(
        result
            .stdout
            .contains(&format!("raw mic: {}", raw.display())),
        "{}",
        result.stdout
    );
    let file = steno_audio::writer::CafFile::read(&raw).unwrap();
    assert_eq!(file.channels.len(), 1);
    assert!(
        file.channels[0].len() >= 4_800,
        "{} frames",
        file.channels[0].len()
    );

    let without = steno(
        &[
            "record",
            "--backend",
            "synthetic",
            "--mode",
            "call",
            "--seconds",
            "0.2",
            "--out",
            out.to_str().unwrap(),
            "--quiet",
        ],
        home,
    );
    assert_eq!(without.status, 0, "{}", without.stderr);
    assert!(!without.stdout.contains("raw mic:"), "{}", without.stdout);
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
