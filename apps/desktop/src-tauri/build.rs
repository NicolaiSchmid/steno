//! Three jobs before `tauri_build`: generate the fixture module the
//! `fixture-host` feature embeds, stand in for the web `dist/` when it has
//! not been built so `cargo build` works on a bare checkout, and let
//! `tauri_build` do its own work (configuration, ACL, Windows resources).

use std::{
    env,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let web = manifest_dir.join("..").join("..").join("macos").join("web");

    generate_fixtures_module(
        &web.join("fixtures").join("bridge"),
        &out_dir.join("fixtures.rs"),
    );
    ensure_frontend_dist(&web.join("dist"), &out_dir);

    tauri_build::build();
}

/// Writes `fixtures.rs`: every file `index.json` lists as a `(key, json)`
/// pair through `include_str!`, so the binary carries the recorded contract
/// and a fixture change recompiles the crate.
fn generate_fixtures_module(fixtures: &Path, target: &Path) {
    println!("cargo:rerun-if-changed={}", fixtures.display());
    let index = fs::read_to_string(fixtures.join("index.json"))
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", fixtures.display()));
    let keys: Vec<String> = serde_json::from_str(&index).expect("index.json is a list of keys");

    let mut module = String::from(
        "/// Every recorded fixture by key, in `index.json` order.\n\
         pub static FIXTURES: &[(&str, &str)] = &[\n",
    );
    for key in keys {
        let path = fixtures.join(format!("{key}.json"));
        assert!(
            path.is_file(),
            "index.json names {key} but {} is missing",
            path.display()
        );
        writeln!(
            module,
            "    ({key:?}, include_str!({:?})),",
            path.display().to_string()
        )
        .expect("write to a String");
    }
    module.push_str("];\n");
    fs::write(target, module).expect("write fixtures.rs");
}

/// `tauri::generate_context!` embeds `frontendDist` and fails when the
/// directory is missing. Without a web build, point it at a one-page
/// placeholder in `OUT_DIR` through `TAURI_CONFIG` (merged into any value
/// the Tauri CLI already set) so the crate still builds and the window says
/// what to run. Once `dist/index.html` exists the override is dropped.
fn ensure_frontend_dist(dist: &Path, out_dir: &Path) {
    println!("cargo:rerun-if-changed={}", dist.display());
    println!("cargo:rerun-if-env-changed=TAURI_CONFIG");
    if dist.join("index.html").is_file() {
        return;
    }
    let placeholder = out_dir.join("web-placeholder");
    fs::create_dir_all(&placeholder).expect("create the placeholder directory");
    fs::write(
        placeholder.join("index.html"),
        "<!doctype html><meta charset=\"utf-8\"><title>Steno</title>\
         <body style=\"font: 14px system-ui; padding: 2rem\">\
         <p>The web UI is not built. Run <code>pnpm install && pnpm build</code> in \
         <code>apps/macos/web</code>, then build steno-desktop again.</p>",
    )
    .expect("write the placeholder page");

    let mut config: serde_json::Value = env::var("TAURI_CONFIG")
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    config["build"]["frontendDist"] = serde_json::Value::String(placeholder.display().to_string());
    println!(
        "cargo:warning=steno-desktop: {} is missing; embedding a placeholder page",
        dist.display()
    );
    println!("cargo:rustc-env=TAURI_CONFIG={config}");
}
