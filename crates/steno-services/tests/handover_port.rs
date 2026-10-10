//! The app's listener, built as the shell builds it (`build`, outside
//! `cfg(test)`), listens on the stored `handoverPort`, or on
//! `STENO_HANDOVER_PORT` when it is set (stable plan X4). It publishes a
//! Bonjour record while it runs. Each case runs in a child process of this
//! test binary with a temporary home, so neither the session's variable
//! nor the developer's support directory (and its handover inbox) reaches
//! it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use steno_core::StenoPaths;
use steno_handover::ListenerState;

/// Set in the child to the port it stores.
const CHILD: &str = "STENO_HANDOVER_PORT_TEST_STORED";
const NAME: &str = "the_app_listens_on_the_stored_handover_port";

/// A port no one listens on now.
fn free_port() -> u16 {
    std::net::TcpListener::bind("0.0.0.0:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// The port the child's app listens on, with `stored` stored and
/// `variable` as `STENO_HANDOVER_PORT`.
fn listening_port(home: &Path, stored: u16, variable: Option<u16>) -> u16 {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", NAME, "--nocapture", "--test-threads=1"])
        .env(CHILD, stored.to_string())
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join("data"))
        .env("APPDATA", home.join("AppData"))
        .env_remove(steno_services::handover::PORT_VARIABLE);
    if let Some(variable) = variable {
        command.env(
            steno_services::handover::PORT_VARIABLE,
            variable.to_string(),
        );
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "the child failed: {stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The harness prints the test's name on the same line, before it.
    stdout
        .split_once("listening=")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("the child printed no port: {stdout}"))
        .parse()
        .unwrap()
}

#[test]
fn the_app_listens_on_the_stored_handover_port() {
    if let Some(stored) = std::env::var_os(CHILD) {
        listen_as_the_shell_does(stored.to_str().unwrap().parse().unwrap());
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let stored = free_port();
    assert_eq!(listening_port(home.path(), stored, None), stored);
    let variable = free_port();
    assert_eq!(
        listening_port(home.path(), stored, Some(variable)),
        variable,
        "the variable wins over the stored port"
    );
}

/// In the child: the default support directory, under the temporary home,
/// with `stored` as `handoverPort`, and the app built on it; prints the
/// port its listener bound.
fn listen_as_the_shell_does(stored: u16) {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let paths = StenoPaths::create_default().unwrap();
    assert!(
        paths.support_directory.starts_with(&home),
        "{} is not under the temporary home",
        paths.support_directory.display()
    );
    {
        let store = steno_services::open_store(&paths.database_path()).unwrap();
        let mut settings = store.settings().unwrap();
        settings.handover_port = Some(stored);
        store.save_settings(&settings).unwrap();
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let app = steno_services::build(steno_services::AppOptions {
            paths,
            database_path: None,
            keyring: false,
            opener: Arc::new(steno_host::fakes::FakeOpener::default()),
            login_item: None,
            update_source: None,
            runtime: tokio::runtime::Handle::current(),
            version: "0.0.0".to_owned(),
            make_capture_session: Arc::new(|_| Err("no capture in this test".to_owned())),
            detection: None,
            lock_patience: std::time::Duration::ZERO,
        })
        .unwrap();
        let service = app.handover.as_ref().unwrap().listener().unwrap().clone();
        service.start().await.unwrap();
        let ListenerState::Listening { port } = service.state() else {
            panic!("not listening: {:?}", service.state());
        };
        println!("listening={port}");
        service.stop().await;
        app.shutdown();
    });
}
