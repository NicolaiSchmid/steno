//! The app's listener, built as the shell builds it (`build`, outside
//! `cfg(test)`), listens on the stored `handoverPort`, or on
//! `STENO_HANDOVER_PORT` when it is set (stable plan X4). It publishes a
//! Bonjour record while it runs, named `Steno port test` (`HOSTNAME`) on
//! Linux and Windows. On the Mac the computer name comes first, so there
//! it can shadow a running Steno's record. Each case runs in a child
//! process of this test binary with a temporary home, so neither the
//! session's variable nor the developer's support directory (and its
//! handover inbox) reaches it.

use std::path::Path;
use std::sync::Arc;

use steno_handover::ListenerState;
use steno_services::handover::PORT_VARIABLE;

mod common;

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
    let mut command = common::child_in(home, NAME);
    command
        .env(CHILD, stored.to_string())
        .env("HOSTNAME", "Steno port test")
        .env_remove(PORT_VARIABLE);
    if let Some(variable) = variable {
        command.env(PORT_VARIABLE, variable.to_string());
    }
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    common::printed(&stdout, "listening=").parse().unwrap()
}

#[test]
fn the_app_listens_on_the_stored_handover_port() {
    if let Ok(stored) = std::env::var(CHILD) {
        listen_as_the_shell_does(stored.parse().unwrap());
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let stored = free_port();
    assert_eq!(listening_port(home.path(), stored, None), stored);
    let variable = std::iter::repeat_with(free_port)
        .find(|&port| port != stored)
        .unwrap();
    assert_eq!(
        listening_port(home.path(), stored, Some(variable)),
        variable,
        "the variable wins over the stored port"
    );
}

/// In the child, once [`common::assert_temporary_home`] passes: the
/// shell's options (`AppOptions::product`, without the keyring) with
/// `stored` as `handoverPort`, and the app built on them. Prints the port
/// its listener bound.
fn listen_as_the_shell_does(stored: u16) {
    common::assert_temporary_home();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let opener = Arc::new(steno_host::fakes::FakeOpener::default());
    let options = steno_services::AppOptions {
        keyring: false,
        ..steno_services::AppOptions::product(runtime.handle().clone(), opener, "0.0.0").unwrap()
    };
    {
        let store = steno_services::open_store(&options.paths.database_path()).unwrap();
        let mut settings = store.settings().unwrap();
        settings.handover_port = Some(stored);
        store.save_settings(&settings).unwrap();
    }
    runtime.block_on(async {
        let app = steno_services::build(options).unwrap();
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
