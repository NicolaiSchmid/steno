//! One models directory for one environment: the app and the CLI
//! (`steno_services::speech`) and the `transcribe` example and the FLEURS
//! test (`steno_speech::ModelStore::from_environment`) find the ONNX
//! models in the same place. The environment is the process's, so each
//! case runs in a child process of this test binary.

use std::path::{Path, PathBuf};

use steno_core::{Settings, StenoPaths};

const CHILD: &str = "STENO_MODELS_DIRECTORY_CHILD";
const NAME: &str = "the_app_the_cli_and_the_example_read_one_models_directory";

/// The two roots the child resolved: the app's, then the example's.
fn roots_in(cwd: &Path, home: &Path, variable: Option<&str>) -> (PathBuf, PathBuf) {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
        .current_dir(cwd)
        .env(CHILD, "1")
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join("data"))
        .env("APPDATA", home.join("appdata"))
        .env_remove(steno_speech::ModelStore::ENVIRONMENT_VARIABLE);
    if let Some(variable) = variable {
        command.env(steno_speech::ModelStore::ENVIRONMENT_VARIABLE, variable);
    }
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let root = |label: &str| {
        stdout
            .lines()
            // The harness may print the test's name on the same line first.
            .find_map(|line| line.split_once(label).map(|(_, path)| PathBuf::from(path)))
            .unwrap_or_else(|| panic!("no {label} in {stdout}"))
    };
    (root("app="), root("example="))
}

#[test]
fn the_app_the_cli_and_the_example_read_one_models_directory() {
    if std::env::var_os(CHILD).is_some() {
        let paths = StenoPaths::new(StenoPaths::default_support_directory());
        let app = steno_services::speech::speech_store_under(
            &steno_services::speech::models_directory(&Settings::default(), &paths),
        );
        println!("app={}", app.root().display());
        println!(
            "example={}",
            steno_speech::ModelStore::from_environment()
                .root()
                .display()
        );
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (cwd, home) = (dir.path().join("work"), dir.path().join("home"));
    std::fs::create_dir_all(&cwd).unwrap();
    let cwd = cwd.canonicalize().unwrap();

    let (app, example) = roots_in(&cwd, &home, Some("relative/models"));
    assert_eq!(app, example);
    assert_eq!(app, cwd.join("relative").join("models").join("onnx"));

    let absolute = dir.path().join("absolute-models");
    let (app, example) = roots_in(&cwd, &home, Some(absolute.to_str().unwrap()));
    assert_eq!(app, example);
    assert_eq!(app, absolute.join("onnx"));

    let (app, example) = roots_in(&cwd, &home, None);
    assert_eq!(app, example, "the default models directory");
    assert!(
        app.ends_with(Path::new("Models").join("onnx")),
        "{}",
        app.display()
    );
}
