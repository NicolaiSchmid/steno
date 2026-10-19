//! The Linux half of `packaged`: the autostart entry's stable path, and
//! the entry an earlier build wrote, which goes while the system manages
//! the login item.

use std::ffi::OsStr;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The variable a package's wrapper sets to the path the autostart entry
/// names, when the launcher is not beside the binary (the AUR wrapper).
pub const EXEC_PATH_VARIABLE: &str = "STENO_EXEC_PATH";

/// Why turning Launch at login on fails when no launcher lies at a path
/// that outlives an upgrade ([`write_entry`]).
pub const NO_STABLE_PATH: &str = "Steno can't open at login from where it's installed now. Restart Steno, or install it with your package manager.";

const BINARY: &str = "steno-desktop";

/// Where Nix keeps its builds, which a garbage collection removes.
const STORE: &str = "/nix/store/";

/// Where a package puts the app's launcher, in the order tried:
/// `/usr/bin`, `/usr/local/bin`, the NixOS system profile, the per-user
/// profile, the user's Nix profile, and the one under `state_home`.
fn candidates(user: Option<&str>, home: Option<&Path>, state_home: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = vec![
        Path::new("/usr/bin").join(BINARY),
        Path::new("/usr/local/bin").join(BINARY),
        Path::new("/run/current-system/sw/bin").join(BINARY),
    ];
    if let Some(user) = user.filter(|user| !user.is_empty() && !user.contains('/')) {
        paths.push(
            Path::new("/etc/profiles/per-user")
                .join(user)
                .join("bin")
                .join(BINARY),
        );
    }
    if let Some(home) = home {
        paths.push(home.join(".nix-profile/bin").join(BINARY));
    }
    if let Some(state_home) = state_home {
        paths.push(state_home.join("nix/profile/bin").join(BINARY));
    }
    paths
}

fn candidates_here() -> Vec<PathBuf> {
    let user = std::env::var("USER").ok();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let state_home = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| home.as_ref().map(|home| home.join(".local/state")));
    candidates(user.as_deref(), home.as_deref(), state_home.as_deref())
}

/// The first of `candidates` that, with every link resolved, lies in the
/// directory `current_exe` resolves into; the candidate itself is returned.
/// A ` (deleted)` suffix on `current_exe`, left by an in-place upgrade, is
/// ignored.
fn stable_path(candidates: &[PathBuf], current_exe: &Path) -> Option<PathBuf> {
    let current_exe = current_exe
        .to_str()
        .and_then(|path| path.strip_suffix(" (deleted)"))
        .map_or(current_exe, Path::new);
    let resolved = current_exe.canonicalize().ok()?;
    let directory = resolved.parent()?;
    candidates
        .iter()
        .find(|candidate| {
            candidate
                .canonicalize()
                .is_ok_and(|target| target.parent() == Some(directory))
        })
        .cloned()
}

/// Whether an `Exec` key may name `path`: absolute, outside the Nix store,
/// UTF-8 without control characters, and with no `%` or `\`, which the XDG
/// autostart generator does not decode before checking the program exists.
fn nameable(path: &Path) -> bool {
    path.is_absolute()
        && !path.starts_with(STORE)
        && path.to_str().is_some_and(|text| {
            !text
                .chars()
                .any(|c| c.is_control() || c == '%' || c == '\\')
        })
}

/// Whether `path` is a regular file, after its links, that someone may run.
fn runnable(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// The path the autostart entry names: [`EXEC_PATH_VARIABLE`]'s value
/// `named` or a candidate; never `current_exe` itself nor a store path.
fn launcher_path_from(
    named: Option<&OsStr>,
    candidates: &[PathBuf],
    current_exe: Option<&Path>,
) -> Option<PathBuf> {
    let launchable = |path: &Path| nameable(path) && runnable(path);
    if let Some(value) = named {
        let path = Path::new(value);
        if launchable(path) {
            return Some(path.to_owned());
        }
        tracing::warn!(
            "{EXEC_PATH_VARIABLE} names no runnable file at an absolute path outside the Nix store; looking for the launcher instead"
        );
    }
    stable_path(candidates, current_exe?).filter(|path| launchable(path))
}

fn launcher_path() -> Option<PathBuf> {
    launcher_path_from(
        std::env::var_os(EXEC_PATH_VARIABLE).as_deref(),
        &candidates_here(),
        std::env::current_exe().ok().as_deref(),
    )
}

/// The autostart entry of `app_name` under `home`, where the plugin reads
/// it.
fn entry_path(app_name: &str, home: &Path) -> PathBuf {
    home.join(".config/autostart")
        .join(format!("{app_name}.desktop"))
}

fn entry_path_here(app_name: &str) -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| entry_path(app_name, Path::new(&home)))
}

fn entry(app_name: &str, exec: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nVersion=1.0\nName={app_name}\nComment={app_name} startup script\nExec={}\nStartupNotify=false\nTerminal=false\n",
        exec_value(&exec.to_string_lossy())
    )
}

/// `path` as the program of an `Exec` key: quoted and escaped as the
/// Desktop Entry Specification says when it holds more than plain
/// characters, so [`is_earlier_entry`]'s comparison stays exact.
fn exec_value(path: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "/._-+".contains(c);
    if path.chars().all(plain) {
        return path.to_owned();
    }
    let mut quoted = String::from("\"");
    for c in path.chars() {
        match c {
            '"' | '`' | '$' => {
                quoted.push_str("\\\\");
                quoted.push(c);
            }
            '\\' => quoted.push_str("\\\\\\\\"),
            '%' => quoted.push_str("%%"),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

/// Writes the autostart entry naming [`launcher_path`], through a
/// temporary file renamed over the old one. Without such a path it fails
/// with [`NO_STABLE_PATH`] and writes nothing.
pub fn write_entry(app_name: &str) -> io::Result<()> {
    let path = entry_path_here(app_name)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    write_entry_at(&path, app_name, launcher_path().as_deref())
}

fn write_entry_at(path: &Path, app_name: &str, exec: Option<&Path>) -> io::Result<()> {
    let Some(exec) = exec else {
        tracing::warn!(
            "Launch at login not turned on: no launcher of this build at a path that outlives an upgrade"
        );
        return Err(io::Error::new(io::ErrorKind::NotFound, NO_STABLE_PATH));
    };
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::other("the entry has no directory"))?;
    std::fs::create_dir_all(directory)?;
    // Hidden, so the XDG autostart generator never reads it as an entry.
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temporary = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let written = std::fs::write(&temporary, entry(app_name, exec))
        .and_then(|()| std::fs::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

fn exec_of(entry: &str) -> Option<&str> {
    entry
        .lines()
        .find_map(|line| line.strip_prefix("Exec="))
        .map(str::trim)
}

/// Whether `entry` is one an earlier build wrote: its `Exec` names a store
/// program or one of the `candidates` as [`write_entry`] writes it.
fn is_earlier_entry(entry: &str, candidates: &[PathBuf]) -> bool {
    exec_of(entry).is_some_and(|exec| {
        exec.trim_start_matches('"').starts_with(STORE)
            || candidates
                .iter()
                .any(|candidate| exec == exec_value(&candidate.to_string_lossy()))
    })
}

/// At launch, while the system manages the login item, removes the
/// autostart entry an earlier build wrote, and only such an entry.
pub fn remove_earlier_entry(app_name: &str) {
    if super::login_item_is_managed()
        && let Some(path) = entry_path_here(app_name)
    {
        remove_if_earlier(&path, &candidates_here());
    }
}

/// Removes the entry at `path` when it is an earlier build's; true when it
/// did.
fn remove_if_earlier(path: &Path, candidates: &[PathBuf]) -> bool {
    let Ok(entry) = std::fs::read_to_string(path) else {
        return false;
    };
    if !is_earlier_entry(&entry, candidates) {
        return false;
    }
    match std::fs::remove_file(path) {
        Ok(()) => {
            tracing::info!(
                "removed the autostart entry an earlier build wrote: the system starts Steno at login"
            );
            true
        }
        Err(error) => {
            tracing::warn!(%error, "the autostart entry an earlier build wrote could not be removed");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    fn executable(path: &Path, text: &str) {
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A Nix store with this build and an older one, wrapped, and profiles
    /// linking to each, one through a linked `bin`.
    struct NixLayout {
        _dir: tempfile::TempDir,
        current_exe: PathBuf,
        system: PathBuf,
        per_user: PathBuf,
        user_profile: PathBuf,
    }

    fn build(store: &Path, name: &str) -> PathBuf {
        let bin = store.join(name).join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        executable(
            &bin.join(BINARY),
            "#!/bin/sh\nexec .steno-desktop-wrapped\n",
        );
        executable(&bin.join(".steno-desktop-wrapped"), "");
        bin
    }

    fn nix_layout() -> NixLayout {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let store = root.join("nix/store");
        let this = build(&store, "aaaa-steno-desktop-0.12.0");
        let older = build(&store, "bbbb-steno-desktop-0.11.0");

        let system = root.join("run/current-system/sw/bin");
        std::fs::create_dir_all(&system).unwrap();
        symlink(older.join(BINARY), system.join(BINARY)).unwrap();

        let per_user = root.join("etc/profiles/per-user/ada/bin");
        std::fs::create_dir_all(&per_user).unwrap();
        symlink(this.join(BINARY), per_user.join(BINARY)).unwrap();

        let environment = store.join("cccc-user-environment");
        std::fs::create_dir_all(&environment).unwrap();
        symlink(&this, environment.join("bin")).unwrap();
        let home = root.join("home/ada");
        std::fs::create_dir_all(&home).unwrap();
        symlink(&environment, home.join(".nix-profile")).unwrap();

        NixLayout {
            current_exe: this.join(".steno-desktop-wrapped"),
            system: system.join(BINARY),
            per_user: per_user.join(BINARY),
            user_profile: home.join(".nix-profile/bin").join(BINARY),
            _dir: dir,
        }
    }

    #[test]
    fn the_candidates_are_usr_bin_then_the_nix_profiles() {
        let home = Path::new("/home/ada");
        assert_eq!(
            candidates(Some("ada"), Some(home), Some(&home.join(".local/state"))),
            [
                PathBuf::from("/usr/bin/steno-desktop"),
                PathBuf::from("/usr/local/bin/steno-desktop"),
                PathBuf::from("/run/current-system/sw/bin/steno-desktop"),
                PathBuf::from("/etc/profiles/per-user/ada/bin/steno-desktop"),
                PathBuf::from("/home/ada/.nix-profile/bin/steno-desktop"),
                PathBuf::from("/home/ada/.local/state/nix/profile/bin/steno-desktop"),
            ]
        );
        assert_eq!(candidates(Some("../x"), None, None).len(), 3);
        assert_eq!(candidates(Some(""), None, None).len(), 3);
    }

    /// The store binary finds the first profile that links to its own
    /// wrapper, one through a linked `bin` too; profiles that link only to
    /// another build, or name nothing, give no path.
    #[test]
    fn a_nix_build_names_the_profile_that_links_to_it() {
        let layout = nix_layout();
        let missing = PathBuf::from("/nonexistent/steno-desktop");
        for (candidates, found) in [
            (
                vec![
                    layout.system.clone(),
                    layout.per_user.clone(),
                    layout.user_profile.clone(),
                ],
                Some(&layout.per_user),
            ),
            (
                vec![layout.system.clone(), layout.user_profile.clone()],
                Some(&layout.user_profile),
            ),
            (vec![layout.system.clone()], None),
            (vec![missing], None),
        ] {
            assert_eq!(
                stable_path(&candidates, &layout.current_exe).as_ref(),
                found,
                "{candidates:?}"
            );
        }
    }

    /// A binary installed in `/usr/bin` is its own candidate.
    #[test]
    fn a_binary_in_place_names_itself() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("usr/bin");
        std::fs::create_dir_all(&bin).unwrap();
        executable(&bin.join(BINARY), "");
        let elsewhere = dir.path().join("target/debug");
        std::fs::create_dir_all(&elsewhere).unwrap();
        executable(&elsewhere.join(BINARY), "");
        let candidate = [bin.join(BINARY)];
        assert_eq!(
            stable_path(&candidate, &bin.join(BINARY)),
            Some(bin.join(BINARY))
        );
        // A development build elsewhere names nothing.
        assert_eq!(stable_path(&candidate, &elsewhere.join(BINARY)), None);
        // An upgrade replaced the binary in place.
        let replaced = PathBuf::from(format!("{} (deleted)", bin.join(BINARY).display()));
        assert_eq!(stable_path(&candidate, &replaced), Some(bin.join(BINARY)));
    }

    /// What an `Exec` key may name; the store check needs no file.
    #[test]
    fn a_nameable_path_is_absolute_outside_the_store_and_plain() {
        assert!(nameable(Path::new("/usr/bin/steno-desktop")));
        assert!(nameable(Path::new("/home/a b/$x/\"q\"/steno-desktop")));
        assert!(!nameable(Path::new(
            "/nix/store/aaaa-steno-desktop/bin/steno-desktop"
        )));
        assert!(!nameable(Path::new("/nix/store")));
        assert!(!nameable(Path::new("steno-desktop")));
        assert!(!nameable(Path::new("/x/100%/steno-desktop")));
        assert!(!nameable(Path::new("/x/a\\b/steno-desktop")));
        assert!(!nameable(Path::new("/x/a\nExec=/bin/sh")));
        assert!(!nameable(Path::new("/x/a\tb")));
        assert!(nameable(Path::new("/nix/storefront/steno-desktop")));
    }

    #[test]
    fn a_runnable_path_is_a_file_with_an_exec_bit() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = dir.path().join(BINARY);
        std::fs::write(&launcher, "").unwrap();
        assert!(!runnable(&launcher), "not executable");
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(runnable(&launcher));
        assert!(!runnable(dir.path()), "a directory");
        assert!(!runnable(&dir.path().join("missing")));
    }

    /// The variable wins when it names a runnable file outside the store,
    /// else this build's candidate; never the running binary or a store
    /// path.
    #[test]
    fn the_launcher_path_is_the_variable_then_a_candidate_never_the_binary() {
        let layout = nix_layout();
        let candidates = [layout.system.clone(), layout.per_user.clone()];
        let exe = Some(layout.current_exe.as_path());

        assert_eq!(
            launcher_path_from(None, &candidates, exe),
            Some(layout.per_user.clone())
        );
        let named = layout.user_profile.as_os_str();
        assert_eq!(
            launcher_path_from(Some(named), &candidates, exe),
            Some(layout.user_profile.clone())
        );
        for ignored in [
            "relative/steno-desktop",
            "/nix/store/x/bin/steno-desktop",
            "/nonexistent",
        ] {
            assert_eq!(
                launcher_path_from(Some(OsStr::new(ignored)), &candidates, exe),
                Some(layout.per_user.clone()),
                "{ignored}"
            );
        }
        assert_eq!(launcher_path_from(None, &[], exe), None);
        assert_eq!(
            launcher_path_from(None, std::slice::from_ref(&layout.system), exe),
            None
        );
        assert_eq!(launcher_path_from(None, &candidates, None), None);

        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join(BINARY), "").unwrap();
        assert_eq!(
            launcher_path_from(None, &[bin.join(BINARY)], Some(&bin.join(BINARY))),
            None
        );
    }

    #[test]
    fn the_entry_names_the_path_and_quotes_what_needs_it() {
        let entry = entry("steno-desktop", Path::new("/usr/bin/steno-desktop"));
        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert!(entry.contains("\nName=steno-desktop\n"));
        assert!(entry.contains("\nExec=/usr/bin/steno-desktop\n"));
        assert_eq!(
            exec_value("/home/a b/.nix-profile/bin/steno-desktop"),
            "\"/home/a b/.nix-profile/bin/steno-desktop\""
        );
        assert_eq!(exec_value("/x/100%/a$b"), "\"/x/100%%/a\\\\$b\"");
    }

    #[test]
    fn the_entry_is_written_where_the_plugin_reads_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = entry_path("steno-desktop", dir.path());
        assert_eq!(
            path,
            dir.path().join(".config/autostart/steno-desktop.desktop")
        );
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[Desktop Entry]\n").unwrap();
        // A hard link keeps the old bytes: the entry is written atomically.
        let old = dir.path().join("old");
        std::fs::hard_link(&path, &old).unwrap();
        write_entry_at(
            &path,
            "steno-desktop",
            Some(Path::new("/usr/bin/steno-desktop")),
        )
        .unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("\nExec=/usr/bin/steno-desktop\n"));
        assert!(!is_earlier_entry(&written, &[]));
        assert_eq!(std::fs::read_to_string(&old).unwrap(), "[Desktop Entry]\n");
    }

    /// With no launcher at a stable path nothing is written and Launch at
    /// login fails.
    #[test]
    fn no_stable_path_is_an_error_that_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = entry_path("steno-desktop", dir.path());
        let error = write_entry_at(&path, "steno-desktop", None).unwrap_err();
        assert_eq!(error.to_string(), NO_STABLE_PATH);
        assert!(!path.exists());
        assert!(!dir.path().join(".config").exists());
    }

    /// Which entries count as an earlier build's.
    #[test]
    fn an_earlier_entry_starts_the_store_or_names_a_candidate() {
        let profile = PathBuf::from("/etc/profiles/per-user/ada/bin/steno-desktop");
        let spaced = PathBuf::from("/home/a b/.nix-profile/bin/steno-desktop");
        let candidates = [profile.clone(), spaced.clone()];
        let store = Path::new("/nix/store/aaaa-steno-desktop/bin/.steno-desktop-wrapped");
        for earlier in [
            entry("steno-desktop", store),
            "[Desktop Entry]\nExec=\"/nix/store/x/bin/steno-desktop\" \n".to_owned(),
            entry("steno-desktop", &profile),
            entry("steno-desktop", &spaced),
            "[Desktop Entry]\nExec=/etc/profiles/per-user/ada/bin/steno-desktop \n".to_owned(),
        ] {
            assert!(is_earlier_entry(&earlier, &candidates), "{earlier}");
        }
        for other in [
            entry("steno-desktop", Path::new("/opt/steno/steno-desktop")),
            "[Desktop Entry]\nExec=/etc/profiles/per-user/ada/bin/steno-desktop --hidden\n"
                .to_owned(),
            "[Desktop Entry]\nName=/nix/store/x\n".to_owned(),
            String::new(),
        ] {
            assert!(!is_earlier_entry(&other, &candidates), "{other}");
        }
    }

    /// Only an earlier build's entry is removed, on a real file.
    #[test]
    fn only_an_earlier_entry_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = entry_path("steno-desktop", dir.path());
        let store = Path::new("/nix/store/aaaa-steno-desktop/bin/.steno-desktop-wrapped");
        assert!(!remove_if_earlier(&path, &[]), "no entry");
        write_entry_at(&path, "steno-desktop", Some(store)).unwrap();
        assert!(remove_if_earlier(&path, &[]));
        assert!(!path.exists());
        let own = Path::new("/opt/steno/steno-desktop");
        write_entry_at(&path, "steno-desktop", Some(own)).unwrap();
        assert!(!remove_if_earlier(&path, &[]));
        assert!(path.exists(), "an entry no earlier build wrote stays");
    }

    const ENVIRONMENT_CHILD: &str = "STENO_PACKAGED_TEST_CHILD";

    /// The public steps read the environment, in a child of this test
    /// binary run managed and unmanaged.
    #[test]
    fn the_entry_steps_read_this_process_s_environment() {
        if std::env::var_os(ENVIRONMENT_CHILD).is_some() {
            let home = PathBuf::from(std::env::var_os("HOME").unwrap());
            assert_eq!(
                candidates_here(),
                candidates(Some("ada"), Some(&home), Some(&home.join(".local/state")))
            );
            let path = entry_path(BINARY, &home);
            let per_user = Path::new("/etc/profiles/per-user/ada/bin").join(BINARY);
            write_entry_at(&path, BINARY, Some(&per_user)).unwrap();
            remove_earlier_entry(BINARY);
            let managed = super::super::login_item_is_managed();
            assert_eq!(path.exists(), !managed, "managed: {managed}");
            if !managed {
                return;
            }
            // This build's own entry stays.
            write_entry(BINARY).unwrap();
            remove_earlier_entry(BINARY);
            assert!(path.exists(), "this build's own entry stays");
            return;
        }
        for managed in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let launcher = dir.path().join("opt").join(BINARY);
            std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
            executable(&launcher, "");
            let name = "packaged::linux::tests::the_entry_steps_read_this_process_s_environment";
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args(["--exact", name, "--test-threads=1"])
                .env(ENVIRONMENT_CHILD, "1")
                .env("HOME", dir.path())
                .env("USER", "ada")
                .env("XDG_STATE_HOME", "relative/state")
                .env(EXEC_PATH_VARIABLE, &launcher);
            if managed {
                child.env(super::super::LOGIN_ITEM_VARIABLE, "managed");
            } else {
                child.env_remove(super::super::LOGIN_ITEM_VARIABLE);
            }
            let output = child.output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "managed: {managed}\n{stdout}{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
