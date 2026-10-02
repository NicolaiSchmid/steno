//! Two jobs before `tauri_build`: stand in for the web `dist/` when it has
//! not been built so a debug `cargo build` works on a bare checkout (a
//! release build fails instead), then let `tauri_build` do its own work
//! (configuration, ACL, Windows resources).

use std::{
    env,
    path::{Path, PathBuf},
};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    ensure_frontend_dist(&manifest_dir);
    tauri_build::build();
}

/// `tauri::generate_context!` embeds `frontendDist` and fails when the
/// directory is missing. Without a web build, point it at `placeholder/`
/// through `TAURI_CONFIG` (merged into any value the Tauri CLI already set)
/// so the crate still builds and the window says what to run. Once
/// `dist/index.html` exists the override is dropped. A release build gets no
/// stand-in: it fails here, so a release bundle never carries the
/// placeholder.
fn ensure_frontend_dist(manifest_dir: &Path) {
    let dist = manifest_dir.join("../../macos/web/dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    println!("cargo:rerun-if-env-changed=TAURI_CONFIG");
    if dist.join("index.html").is_file() {
        return;
    }
    assert!(
        env::var("PROFILE").as_deref() != Ok("release"),
        "steno-desktop: {} is missing and a release build never embeds the placeholder page; \
         run `pnpm build` in apps/macos/web first",
        dist.display()
    );
    let mut config: serde_json::Value = env::var("TAURI_CONFIG")
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    config["build"]["frontendDist"] = manifest_dir
        .join("placeholder")
        .display()
        .to_string()
        .into();
    println!(
        "cargo:warning=steno-desktop: {} is missing; embedding a placeholder page",
        dist.display()
    );
    println!("cargo:rustc-env=TAURI_CONFIG={config}");
}
