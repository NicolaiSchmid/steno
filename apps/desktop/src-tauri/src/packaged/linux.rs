//! The Linux half of `packaged`: the autostart entry's stable path, and
//! the entry an earlier build wrote, which goes while the system manages
//! the login item.

use std::ffi::OsStr;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// The variable a package's wrapper sets to the path the autostart
/// entry names, for a launcher that is not beside the binary it runs
/// (the AUR package's `/usr/bin` wrapper, stable plan X6).
pub const EXEC_PATH_VARIABLE: &str = "STENO_EXEC_PATH";

/// Why turning Launch at login on fails when no launcher of this build
/// lies at a path that outlives an upgrade ([`write_entry`]); the General
/// section shows it under its error.
pub const NO_STABLE_PATH: &str = "Steno can't open at login from where it's installed now. Restart Steno, or install it with your package manager.";

/// The binary's name, in every package.
const BINARY: &str = "steno-desktop";

/// Where Nix keeps its builds, which a garbage collection removes.
const STORE: &str = "/nix/store/";

/// The unit systemd's XDG autostart generator makes from the entry
/// (`app-<escaped entry name>@autostart.service`).
const AUTOSTART_UNIT: &str = "app-steno\\x2ddesktop@autostart.service";

/// Where a package puts the app's launcher, in the order they are tried:
/// `/usr/bin` (the `.deb`, and a package that installs the binary or a
/// link to it there), `/usr/local/bin`, the NixOS system profile, the
/// per-user profile NixOS and Home Manager fill (for `user`), the user's
/// Nix profile (under `home`) and, with `use-xdg-base-directories`, the
/// one under `state_home` (`$XDG_STATE_HOME`, else `~/.local/state`).
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

/// [`candidates`] for this process's user, `HOME` and `XDG_STATE_HOME`.
fn candidates_here() -> Vec<PathBuf> {
    let user = std::env::var("USER").ok();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let state_home = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| home.as_ref().map(|home| home.join(".local/state")));
    candidates(user.as_deref(), home.as_deref(), state_home.as_deref())
}

/// The first of `candidates` that launches this build: one that exists
/// and, with every link resolved, lies in the directory that
/// `current_exe` resolves into. The candidate itself is returned, not
/// where it resolves to. Once an upgrade has replaced the running binary
/// in place, Linux appends ` (deleted)` to `current_exe`; the path
/// without it is where the new build lies.
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

/// Whether an `Exec` key may name `path`, by its text alone: absolute,
/// outside the Nix store, UTF-8 with no control character (a newline
/// would end the key), and with no `%` or `\`, which systemd's XDG
/// autostart generator does not decode before it checks that the program
/// exists, so it skips the entry.
fn nameable(path: &Path) -> bool {
    path.is_absolute()
        && !path.starts_with(STORE)
        && path.to_str().is_some_and(|text| {
            !text
                .chars()
                .any(|c| c.is_control() || c == '%' || c == '\\')
        })
}

/// Whether `path` is a regular file, after its links, that someone may
/// run (the generator skips one nobody may).
fn runnable(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// The path the autostart entry names (see the module doc of `packaged`),
/// from [`EXEC_PATH_VARIABLE`]'s value `named`, the `candidates` and the
/// running binary; `None` when there is none. Never `current_exe`
/// itself, and never a path in the Nix store.
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

/// [`launcher_path_from`] over this process's environment.
fn launcher_path() -> Option<PathBuf> {
    launcher_path_from(
        std::env::var_os(EXEC_PATH_VARIABLE).as_deref(),
        &candidates_here(),
        std::env::current_exe().ok().as_deref(),
    )
}

/// The autostart entry of `app_name` under `home`, where the plugin
/// reads and removes it.
fn entry_path(app_name: &str, home: &Path) -> PathBuf {
    home.join(".config/autostart")
        .join(format!("{app_name}.desktop"))
}

/// [`entry_path`] under `HOME`; `None` when it is not set.
fn entry_path_here(app_name: &str) -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| entry_path(app_name, Path::new(&home)))
}

/// The entry the plugin writes, naming `exec` with no arguments.
fn entry(app_name: &str, exec: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nVersion=1.0\nName={app_name}\nComment={app_name} startup script\nExec={}\nStartupNotify=false\nTerminal=false\n",
        exec_value(&exec.to_string_lossy())
    )
}

/// `path` as the program of an `Exec` key: as it is when it holds only
/// plain characters, else quoted as the Desktop Entry Specification says
/// (`"`, `` ` ``, `$` and `\` escaped inside the quotes, `%` doubled),
/// then with `\` escaped once more for the string value. The app never
/// names a path with `%` or `\` ([`nameable`]); the branches keep
/// [`is_earlier_entry`]'s comparison exact for any candidate.
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

/// Writes the autostart entry of `app_name` naming [`launcher_path`].
/// With no such path it writes nothing and fails with [`NO_STABLE_PATH`],
/// so the setting is not saved and the General section says why.
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
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)?;
    }
    std::fs::write(path, entry(app_name, exec))
}

/// An entry's `Exec` value, without the spaces around it.
fn exec_of(entry: &str) -> Option<&str> {
    entry
        .lines()
        .find_map(|line| line.strip_prefix("Exec="))
        .map(str::trim)
}

/// Whether `entry` is one an earlier build wrote: its `Exec` starts a
/// program in the Nix store, or is one of the `candidates` as
/// [`write_entry`] writes it.
fn is_earlier_entry(entry: &str, candidates: &[PathBuf]) -> bool {
    exec_of(entry).is_some_and(|exec| {
        exec.trim_start_matches('"').starts_with(STORE)
            || candidates
                .iter()
                .any(|candidate| exec == exec_value(&candidate.to_string_lossy()))
    })
}

/// What happens to the autostart entry at launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Removal {
    /// It stays: the system does not manage the login item, or the entry
    /// is not an earlier build's.
    Keep,
    /// It goes now.
    Now,
    /// It goes at the exit, after the save: the app runs as the unit the
    /// generator made from it, which a reload without the entry would
    /// leave loaded but outside the session, so a logout would not stop
    /// it.
    AtExit,
}

/// What [`at_launch`] does with the entry: see [`Removal`].
fn removal(managed: bool, earlier: bool, as_autostart_unit: bool) -> Removal {
    match (managed && earlier, as_autostart_unit) {
        (false, _) => Removal::Keep,
        (true, false) => Removal::Now,
        (true, true) => Removal::AtExit,
    }
}

/// Set at launch when an earlier build's entry waits for the exit
/// ([`Removal::AtExit`]).
static EARLIER_ENTRY_AT_EXIT: AtomicBool = AtomicBool::new(false);

/// At launch: while the system manages the login item
/// (`packaged::login_item_is_managed`), the autostart entry of `app_name`
/// an earlier build wrote ([`is_earlier_entry`]) goes, and only such an
/// entry. While the app runs as the autostart unit
/// ([`runs_as_autostart_unit`]) it goes at the exit instead
/// ([`remove_earlier_entry_at_exit`]).
pub fn remove_earlier_entry(app_name: &str) {
    let Some(path) = entry_path_here(app_name) else {
        return;
    };
    let removal = at_launch(
        &path,
        super::login_item_is_managed(),
        &candidates_here(),
        runs_as_autostart_unit(),
    );
    if removal == Removal::AtExit {
        EARLIER_ENTRY_AT_EXIT.store(true, Ordering::Relaxed);
    }
}

/// [`remove_earlier_entry`]'s launch step for the entry at `path`.
fn at_launch(
    path: &Path,
    managed: bool,
    candidates: &[PathBuf],
    as_autostart_unit: bool,
) -> Removal {
    let earlier =
        std::fs::read_to_string(path).is_ok_and(|entry| is_earlier_entry(&entry, candidates));
    let removal = removal(managed, earlier, as_autostart_unit);
    match removal {
        Removal::Keep => {}
        Removal::Now => {
            remove_if_earlier(path, candidates);
        }
        Removal::AtExit => tracing::info!(
            "the autostart entry an earlier build wrote goes when Steno exits: Steno runs as the unit made from it"
        ),
    }
    removal
}

/// After the shutdown of an exit: the entry [`remove_earlier_entry`] left
/// for the exit goes, if it is still an earlier build's. Not for an
/// update's relaunch (`relaunching`), whose next process runs on in the
/// same unit. A kill before this leaves the entry for the next launch.
/// The one save that may not end the process is the one at an Xfce query
/// on Wayland, which relaunches the app when the session goes on
/// (`session_end::SaveAndQuit::of`); xfce4-session starts autostart
/// entries itself, so that app is not the autostart unit and never
/// leaves the entry for the exit.
pub fn remove_earlier_entry_at_exit(app_name: &str, relaunching: bool) {
    if !removes_at_exit(EARLIER_ENTRY_AT_EXIT.load(Ordering::Relaxed), relaunching) {
        return;
    }
    if let Some(path) = entry_path_here(app_name) {
        remove_if_earlier(&path, &candidates_here());
    }
}

/// Whether the exit removes the entry: it waited for the exit
/// (`deferred`), and the exit is not an update's relaunch.
fn removes_at_exit(deferred: bool, relaunching: bool) -> bool {
    deferred && !relaunching
}

/// Removes the entry at `path` when it is an earlier build's; true when
/// it did.
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

/// Whether this process runs in the autostart unit (`AUTOSTART_UNIT`):
/// a component of its cgroup's path in `/proc/self/cgroup` is that unit.
/// False when the file cannot be read.
fn runs_as_autostart_unit() -> bool {
    std::fs::read_to_string("/proc/self/cgroup")
        .is_ok_and(|cgroup| runs_in(&cgroup, AUTOSTART_UNIT))
}

/// Whether a cgroup listing, as `/proc/<pid>/cgroup` holds it (one
/// `<id>:<controllers>:<path>` line per hierarchy), puts the process in
/// `unit` or below it.
fn runs_in(cgroup: &str, unit: &str) -> bool {
    cgroup
        .lines()
        .filter_map(|line| line.splitn(3, ':').nth(2))
        .any(|path| path.split('/').any(|component| component == unit))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    /// Writes a file anyone may run.
    fn executable(path: &Path, text: &str) {
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A store with this build and an older one, each a wrapper beside
    /// the wrapped binary; the system profile links to the older build,
    /// the per-user profile to this one, and the user's profile is a
    /// linked tree whose `bin` is itself a link (as `buildEnv` makes for a
    /// single package).
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

    /// The wrapped binary in the store finds the profile that links to
    /// its own wrapper, skipping a profile that links to another build,
    /// and the path returned is the profile's, never the store's.
    #[test]
    fn a_nix_build_names_the_profile_that_links_to_it() {
        let layout = nix_layout();
        let found = stable_path(
            &[
                layout.system.clone(),
                layout.per_user.clone(),
                layout.user_profile.clone(),
            ],
            &layout.current_exe,
        );
        assert_eq!(found.as_ref(), Some(&layout.per_user));
        assert!(!found.unwrap().to_string_lossy().contains("/nix/store/"));
    }

    /// A profile whose `bin` directory is itself a link counts too.
    #[test]
    fn a_profile_whose_bin_is_a_link_counts() {
        let layout = nix_layout();
        let found = stable_path(
            &[layout.system.clone(), layout.user_profile.clone()],
            &layout.current_exe,
        );
        assert_eq!(found, Some(layout.user_profile));
    }

    /// Only profiles that link to another build, or none at all: no
    /// path, so no entry.
    #[test]
    fn no_profile_of_this_build_means_no_path() {
        let layout = nix_layout();
        assert_eq!(
            stable_path(std::slice::from_ref(&layout.system), &layout.current_exe),
            None
        );
        assert_eq!(
            stable_path(
                &[PathBuf::from("/nonexistent/steno-desktop")],
                &layout.current_exe
            ),
            None
        );
    }

    /// The `.deb` and a package that installs the binary itself in
    /// `/usr/bin`: the candidate is the running binary.
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

    /// What an `Exec` key may name, by its text: the store check holds
    /// for a path that need not exist.
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

    /// The launcher path from the variable, the candidates and the
    /// running binary: the variable wins when it names a runnable file
    /// outside the store; else the candidate of this build; never the
    /// running binary itself, nor a path in the store.
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

        // A candidate nobody may run is no launcher.
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
        write_entry_at(
            &path,
            "steno-desktop",
            Some(Path::new("/usr/bin/steno-desktop")),
        )
        .unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("\nExec=/usr/bin/steno-desktop\n"));
        assert!(!is_earlier_entry(&written, &[]));
    }

    /// With no launcher at a stable path nothing is written, and turning
    /// Launch at login on fails with the words the General section shows.
    #[test]
    fn no_stable_path_is_an_error_that_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = entry_path("steno-desktop", dir.path());
        let error = write_entry_at(&path, "steno-desktop", None).unwrap_err();
        assert_eq!(error.to_string(), NO_STABLE_PATH);
        assert!(!path.exists());
        assert!(!dir.path().join(".config").exists());
    }

    /// An earlier build's entry starts a program in the store or names a
    /// candidate as this build writes it; another entry, or any other
    /// text, is not one.
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

    /// Only while managed does an earlier entry go: now, or at the exit
    /// while the app runs as the unit made from it.
    #[test]
    fn an_earlier_entry_goes_only_while_managed_and_at_the_exit_as_the_unit() {
        assert_eq!(removal(true, true, false), Removal::Now);
        assert_eq!(removal(true, true, true), Removal::AtExit);
        for as_unit in [false, true] {
            assert_eq!(removal(false, true, as_unit), Removal::Keep);
            assert_eq!(removal(true, false, as_unit), Removal::Keep);
            assert_eq!(removal(false, false, as_unit), Removal::Keep);
        }
        assert!(removes_at_exit(true, false));
        assert!(
            !removes_at_exit(true, true),
            "an update's relaunch keeps it"
        );
        assert!(!removes_at_exit(false, false));
    }

    /// The launch step on a real file: outside the unit a store entry
    /// goes at once; as the unit it stays through the launch and goes at
    /// the exit step; an entry no earlier build wrote, or an unmanaged
    /// launch, keeps it.
    #[test]
    fn a_store_entry_waits_for_the_exit_as_the_autostart_unit() {
        let dir = tempfile::tempdir().unwrap();
        let path = entry_path("steno-desktop", dir.path());
        let store = Path::new("/nix/store/aaaa-steno-desktop/bin/.steno-desktop-wrapped");
        let write = |exec: &Path| write_entry_at(&path, "steno-desktop", Some(exec)).unwrap();

        assert_eq!(
            at_launch(&path, true, &[], false),
            Removal::Keep,
            "no entry"
        );

        write(store);
        assert_eq!(at_launch(&path, false, &[], false), Removal::Keep);
        assert!(path.exists(), "an unmanaged launch keeps it");
        assert_eq!(at_launch(&path, true, &[], false), Removal::Now);
        assert!(!path.exists(), "outside the unit it goes at once");

        write(store);
        assert_eq!(at_launch(&path, true, &[], true), Removal::AtExit);
        assert!(path.exists(), "as the unit it stays while the app runs");
        assert!(removes_at_exit(true, false));
        assert!(remove_if_earlier(&path, &[]));
        assert!(!path.exists(), "gone after the exit");

        write(Path::new("/opt/steno/steno-desktop"));
        assert_eq!(at_launch(&path, true, &[], false), Removal::Keep);
        assert!(!remove_if_earlier(&path, &[]));
        assert!(path.exists(), "an entry no earlier build wrote stays");
    }

    /// Set in the child [`the_entry_steps_read_this_process_s_environment`]
    /// runs.
    const ENVIRONMENT_CHILD: &str = "STENO_PACKAGED_TEST_CHILD";

    /// The public steps over the environment: `USER`, `HOME`,
    /// `XDG_STATE_HOME` (a relative one falls back to `~/.local/state`),
    /// [`EXEC_PATH_VARIABLE`] and the managed variable, in a child of this
    /// test binary run once managed and once not, with `HOME` in a temp
    /// directory.
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
            // As the unit: the exit step re-reads the entry first, and an
            // update's relaunch keeps it.
            EARLIER_ENTRY_AT_EXIT.store(true, Ordering::Relaxed);
            write_entry(BINARY).unwrap();
            remove_earlier_entry_at_exit(BINARY, false);
            assert!(path.exists(), "this build's own entry stays");
            write_entry_at(&path, BINARY, Some(&per_user)).unwrap();
            remove_earlier_entry_at_exit(BINARY, true);
            assert!(path.exists(), "an update's relaunch keeps it");
            remove_earlier_entry_at_exit(BINARY, false);
            assert!(!path.exists(), "gone after the exit");
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

    #[test]
    fn the_cgroup_names_the_autostart_unit() {
        let unit = AUTOSTART_UNIT;
        let v2 = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-steno\\x2ddesktop@autostart.service\n";
        assert!(runs_in(v2, unit));
        let uwsm = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-graphical.slice/app-steno\\x2ddesktop@autostart.service/sub\n";
        assert!(runs_in(uwsm, unit), "a cgroup below the unit is in it");
        let hybrid = "12:cpu,cpuacct:/\n1:name=systemd:/user.slice/user-1000.slice/user@1000.service/app.slice/app-steno\\x2ddesktop@autostart.service\n0::/\n";
        assert!(runs_in(hybrid, unit));
        let service = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/steno.service\n";
        assert!(!runs_in(service, unit));
        let other = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-steno\\x2ddesktop@autostart.service.bak\n";
        assert!(!runs_in(other, unit));
        assert!(!runs_in("", unit));
    }

    /// On this process's real cgroup listing: the test runner is not the
    /// autostart unit.
    #[test]
    fn the_test_runner_is_not_the_autostart_unit() {
        let cgroup = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
        assert!(!runs_as_autostart_unit(), "{cgroup}");
    }
}
