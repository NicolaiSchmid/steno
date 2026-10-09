//! Linux only: the real sidecar in a systemd scope of its own, asked of a
//! real systemd user manager (P6 of `.plans/2026-10-07-stable-promotion.md`,
//! `crates/steno-speech/src/sidecar/scope.rs`). The client's own tests run
//! against a fake manager; this one reads this process's cgroup and
//! `$XDG_RUNTIME_DIR` and the child's `/proc` entries as the app does.
//!
//! Ignored by default: it needs a user manager and runs in a unit of
//! one, as CI's Linux job runs it:
//! `systemd-run --user --scope cargo test -p steno-speech-sidecar --test user_scope -- --ignored`.

#![cfg(target_os = "linux")]

mod common;

use std::process::Command;

use common::{alive, config, engine_in, within_ten_seconds};
use steno_core::SpeechEngine as _;

/// `systemctl --user` with `args`, its output.
fn systemctl(args: &[&str]) -> String {
    let output = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .unwrap();
    String::from_utf8(output.stdout).unwrap()
}

/// The last part of the cgroup path in the cgroup file of `pid`.
fn unit_of(pid: &str) -> String {
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap();
    let path = cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .unwrap();
    path.rsplit('/').next().unwrap().to_owned()
}

/// The sidecar's scopes the user manager knows, in any state.
fn sidecar_scopes() -> String {
    systemctl(&[
        "list-units",
        "--all",
        "--plain",
        "--no-legend",
        "app-steno*",
    ])
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a systemd user manager: run it under `systemd-run --user --scope`"]
async fn the_sidecar_runs_in_a_scope_of_its_own_beside_the_apps_unit() {
    let own = unit_of("self");
    assert!(
        own.rsplit('.').next() == Some("scope")
            && systemctl(&["show", "-p", "ActiveState", "--value", &own]).trim() == "active",
        "not in a scope of a user manager ({own}): run the test under `systemd-run --user --scope`"
    );
    assert_eq!(
        sidecar_scopes(),
        "",
        "a sidecar's scope was left before the test"
    );

    let dir = tempfile::tempdir().unwrap();
    let engine = engine_in(&dir, config(&[]));
    engine.prepare().await.unwrap();
    let pid = engine.pid().unwrap();
    let scope = format!("app-steno\\x2dspeech\\x2dsidecar-{pid}.scope");
    assert_eq!(unit_of(&pid.to_string()), scope);
    let show = |property: &str| {
        systemctl(&["show", "-p", property, "--value", &scope])
            .trim()
            .to_owned()
    };
    assert_eq!(show("ActiveState"), "active");
    assert_eq!(show("PartOf"), own);
    assert_eq!(
        show("Slice"),
        systemctl(&["show", "-p", "Slice", "--value", &own]).trim()
    );

    engine.shut_down().await.unwrap();
    assert!(within_ten_seconds(|| (!alive(pid)).then_some(())).is_some());
    assert!(
        within_ten_seconds(|| sidecar_scopes().is_empty().then_some(())).is_some(),
        "the sidecar's scope was left: {}",
        sidecar_scopes()
    );
}
