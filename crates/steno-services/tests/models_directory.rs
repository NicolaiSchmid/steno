//! One models directory for one environment: the app and the CLI
//! (`steno_services::speech`) and the `transcribe` example and the FLEURS
//! test (`steno_speech::ModelStore::from_environment`) find the ONNX
//! models in the same place, and `STENO_MODELS_MIRROR` reaches the app's
//! speech setup. The environment is the process's, so each case runs in a
//! child process of this test binary.

use std::path::{Path, PathBuf};

use steno_core::{Settings, StenoPaths};

const CHILD: &str = "STENO_MODELS_DIRECTORY_CHILD";
const NAME: &str = "the_app_the_cli_and_the_example_read_one_models_directory";

/// The test `name` run as a child in `cwd` with `home` as its home and
/// `variables` set; what it printed after `label`, per label.
fn child(
    name: &str,
    cwd: &Path,
    home: &Path,
    variables: &[(&str, &str)],
    labels: &[&str],
) -> Vec<String> {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .current_dir(cwd)
        .env(CHILD, "1")
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join("data"))
        .env("APPDATA", home.join("appdata"))
        .env_remove(steno_speech::ModelStore::ENVIRONMENT_VARIABLE)
        .env_remove(steno_speech::ModelStore::MIRROR_ENVIRONMENT_VARIABLE)
        .envs(variables.iter().copied());
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    labels
        .iter()
        .map(|label| {
            stdout
                .lines()
                // The harness may print the test's name on the same line first.
                .find_map(|line| line.split_once(label).map(|(_, value)| value.to_owned()))
                .unwrap_or_else(|| panic!("no {label} in {stdout}"))
        })
        .collect()
}

/// The two roots the child resolved: the app's, then the example's.
fn roots_in(cwd: &Path, home: &Path, variable: Option<&str>) -> (PathBuf, PathBuf) {
    let variables: Vec<_> = variable
        .map(|value| (steno_speech::ModelStore::ENVIRONMENT_VARIABLE, value))
        .into_iter()
        .collect();
    let roots = child(NAME, cwd, home, &variables, &["app=", "example="]);
    (PathBuf::from(&roots[0]), PathBuf::from(&roots[1]))
}

#[test]
fn the_app_the_cli_and_the_example_read_one_models_directory() {
    if std::env::var_os(CHILD).is_some() {
        let paths = StenoPaths::new(StenoPaths::default_support_directory());
        let app = steno_speech::ModelStore::in_models_directory(
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
    // The roots exist, so they compare as the file system sees them: the
    // working directory can come back resolved (`/private/var` on the Mac,
    // a `\\?\` or long name on Windows).
    let same = |left: &Path, right: &Path| {
        assert_eq!(
            left.canonicalize().unwrap(),
            right.canonicalize().unwrap(),
            "{} and {}",
            left.display(),
            right.display()
        );
    };

    let relative = Path::new("relative").join("models");
    let expected = cwd.join(&relative).join("onnx");
    std::fs::create_dir_all(&expected).unwrap();
    let (app, example) = roots_in(&cwd, &home, Some(relative.to_str().unwrap()));
    same(&app, &example);
    same(&app, &expected);

    let absolute = dir.path().join("absolute-models");
    std::fs::create_dir_all(absolute.join("onnx")).unwrap();
    let (app, example) = roots_in(&cwd, &home, Some(absolute.to_str().unwrap()));
    same(&app, &example);
    same(&app, &absolute.join("onnx"));

    let (app, example) = roots_in(&cwd, &home, None);
    assert_eq!(app, example, "the default models directory");
    assert!(
        app.ends_with(Path::new("Models").join("onnx")),
        "{}",
        app.display()
    );
}

const MIRROR: &str = "the_mirror_variable_reaches_the_speech_setup";

/// `STENO_MODELS_MIRROR` sets the mirror of the store the app's speech
/// setup installs into, over an absent `speech.json`.
#[test]
fn the_mirror_variable_reaches_the_speech_setup() {
    if std::env::var_os(CHILD).is_some() {
        let paths = StenoPaths::new(StenoPaths::default_support_directory());
        let setup = steno_services::speech::SpeechSetup::new(&Settings::default(), &paths);
        println!(
            "mirror={}",
            setup.model_store().mirror().unwrap_or("(none)")
        );
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mirror = |variables: &[(&str, &str)]| {
        child(MIRROR, dir.path(), dir.path(), variables, &["mirror="]).remove(0)
    };
    assert_eq!(
        mirror(&[(
            steno_speech::ModelStore::MIRROR_ENVIRONMENT_VARIABLE,
            "http://mirror.example:8000/models/"
        )]),
        "http://mirror.example:8000/models"
    );
    assert_eq!(mirror(&[]), "(none)");
}
